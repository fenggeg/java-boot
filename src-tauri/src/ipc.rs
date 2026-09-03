//! launcher 侧 IPC 客户端（R1 / ADR-0001 决策 1/2）。
//!
//! 职责:UI 是无状态视图，本文只负责与 `javaboot-daemon` 的通信——
//! - 连接命名管道（失败则退避重连）
//! - `daemon.hello` 握手 + 版本协商
//! - 请求/响应（id → oneshot）+ 事件订阅（`log.append` / `proc.status`）
//! - 每 5s 心跳;断连后指数退避重连;重连即触发前端对账
//!
//! 线程模型：单一后台驱动 task 独占连接（读/写），对外暴露 `IpcState`，
//! 前端/命令层经 `request` 发请求、订阅 `events`、读取 `connected`/`hello`。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::ClientOptions;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use jb_core::consts as C;
use jb_core::model::{LogLine, ProcessInfo};
use jb_core::protocol::*;

use crate::error::AppResult;

/// 本 launcher 的应用版本（握手 `client_version`）。
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 一次请求支配的响应通道。
type Reply = oneshot::Sender<std::result::Result<serde_json::Value, RpcError>>;

enum Cmd {
    Request { method: String, params: Option<serde_json::Value>, reply: Reply },
}

/// 服务端主动推送的事件。
#[derive(Clone)]
pub enum IpcEvent {
    Connected(HelloResult),
    Disconnected,
    Log { line: LogLine },
    ProcStatus { run_id: i64, status: String },
    /// P3 监控：单进程资源采样。
    Metrics { run_id: i64, cpu_usage: Option<f32>, memory_mb: Option<f64> },
}

#[derive(Clone)]
pub struct IpcState {
    cmd_tx: mpsc::UnboundedSender<Cmd>,
    pub events: broadcast::Sender<IpcEvent>,
    pub connected: watch::Receiver<bool>,
    pub hello: watch::Receiver<Option<HelloResult>>,
}

impl IpcState {
    /// 创建并启动后台驱动 task（挂到 tauri 全局 runtime）。
    pub fn spawn() -> Arc<Self> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Cmd>();
        let (events, _) = broadcast::channel::<IpcEvent>(4096);
        let (connected_tx, connected) = watch::channel(false);
        let (hello_tx, hello) = watch::channel(None);

        let state = Arc::new(IpcState {
            cmd_tx,
            events,
            connected,
            hello,
        });
        {
            let clone = state.clone();
            tauri::async_runtime::spawn(async move {
                driver(clone, cmd_rx, connected_tx, hello_tx).await;
            });
        }
        state
    }

    /// 单向请求：方法 + 参数 → 反序列化结果。
    pub async fn request<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> AppResult<T> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(Cmd::Request { method: method.to_string(), params: Some(params), reply })
            .map_err(|_| crate::error::AppError::Other("IPC 已关闭".into()))?;
        let resp = rx
            .await
            .map_err(|_| crate::error::AppError::Other("IPC 驱动已断开".into()))?;
        let value = resp.map_err(|e| {
            crate::error::AppError::Other(format!("daemon 错误 {}({})", e.code, e.message))
        })?;
        serde_json::from_value(value)
            .map_err(|e| crate::error::AppError::Other(format!("响应解析失败: {e}")))
    }

    /// 对账：拉取 daemon 全部托管进程事实。
    pub async fn reconcile(&self) -> AppResult<Vec<ProcessInfo>> {
        self.request::<Vec<ProcessInfo>>(method::LIST, serde_json::json!({})).await
    }

    pub fn is_connected(&self) -> bool {
        *self.connected.borrow()
    }

    /// 确保 daemon 就绪（最多等待 `timeout`）：尝试拉起 daemon 并等待握手成立。
    ///
    /// 用于服务启动前的就绪门控——避免「daemon 尚未握手成功时启动服务」被
    /// 静默回退到本地路径，导致同一批服务托管归属不一致。
    pub async fn ensure_daemon_ready(&self, timeout: Duration) -> bool {
        // 已就绪：直接返回
        if self.is_connected() {
            return true;
        }
        // 先尝试拉起 daemon（内部带 2s 冷却，失败/已在跑会自行退出，幂等）
        let _ = spawn_daemon_process();
        // 克隆 connected 接收端，等待握手成立（Receiver 支持 Clone，监听状态变化）
        let mut rx = self.connected.clone();
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if self.is_connected() {
                return true;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                log::warn!("等待 daemon 就绪超时（{}ms），服务将降级本地托管", timeout.as_millis());
                return false;
            }
            // 以短超时轮询监看状态变化；driver 退避重连时 connected 不会频繁变更，
            // 但对每次 changed 施加超时，确保外层 deadline 得以被周期性检查。
            let _ = tokio::time::timeout(Duration::from_millis(100), rx.changed()).await;
        }
    }

    /// 把 daemon 服务端事件转发为 Tauri 前端事件（UI 订阅即得实时状态/日志/指标）。
    pub fn forward_to_frontend(&self, app: tauri::AppHandle) {
        use tauri::Emitter;
        let mut rx = self.events.subscribe();
        tauri::async_runtime::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                let (name, payload) = match ev {
                    IpcEvent::Connected(h) => (
                        "daemon-connected",
                        serde_json::json!({
                            "daemon_version": h.daemon_version,
                            "has_running": h.has_running,
                            "has_pending_recovery": h.has_pending_recovery,
                        }),
                    ),
                    IpcEvent::Disconnected => ("daemon-disconnected", serde_json::json!({})),
                    IpcEvent::Log { line } => ("daemon-log", serde_json::json!(line)),
                    IpcEvent::ProcStatus { run_id, status } => {
                        ("daemon-proc-status", serde_json::json!({ "run_id": run_id, "status": status }))
                    }
                    IpcEvent::Metrics { run_id, cpu_usage, memory_mb } => (
                        "daemon-proc-metrics",
                        serde_json::json!({ "run_id": run_id, "cpu_usage": cpu_usage, "memory_mb": memory_mb }),
                    ),
                };
                let _ = app.emit(name, payload);
            }
        });
    }
}

/// 驱动主循环：连接 → hello → 读写/心跳 → 断连清理 → 退避重连。
async fn driver(
    state: Arc<IpcState>,
    mut cmd_rx: mpsc::UnboundedReceiver<Cmd>,
    connected_tx: watch::Sender<bool>,
    hello_tx: watch::Sender<Option<HelloResult>>,
) {
    let mut backoff = Duration::from_millis(C::RECONNECT_BASE_MS);
    let mut need_full_reconnect = true;

    loop {
        // 建立（或重连）一条连接
        if need_full_reconnect {
            let pipe = match ClientOptions::new().open(C::PIPE_NAME) {
                Ok(p) => p,
                Err(_) => {
                    log::info!("daemon 未就绪，{}ms 后重试", backoff.as_millis());
                    // 断连自愈：尝试重新拉起 daemon（内部带 2s 冷却，不会刷屏）
                    let _ = spawn_daemon_process();
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_millis(C::RECONNECT_MAX_MS));
                    continue;
                }
            };
            backoff = Duration::from_millis(C::RECONNECT_BASE_MS);
            let (rd, mut wr) = tokio::io::split(pipe);
            let mut reader = BufReader::new(rd);

            // hello 握手
            let hello = match handshake(&mut wr, &mut reader).await {
                Some(h) => h,
                None => {
                    let _ = connected_tx.send(false);
                    log::info!("握手失败，重连");
                    continue;
                }
            };
            hello_tx.send(Some(hello.clone())).ok();
            let _ = state.events.send(IpcEvent::Connected(hello));
            let _ = connected_tx.send(true);

            // ---- 已连接：读写/心跳循环 ----
            let mut pending: HashMap<u64, Reply> = HashMap::new();
            let mut next_id = 2u64;
            let mut heartbeat = tokio::time::interval(Duration::from_secs(C::HEARTBEAT_INTERVAL_SECS));
            // 首个 tick 立即触发，先让它跳过
            heartbeat.tick().await;

            let session_ok = run_session(&mut reader, &mut wr, state.clone(), &mut cmd_rx, &mut pending, &mut next_id, &mut heartbeat).await;

            let _ = connected_tx.send(false);
            let _ = state.events.send(IpcEvent::Disconnected);
            for (_, rep) in pending.drain() {
                let _ = rep.send(Err(RpcError::new(ERR_INTERNAL_ERROR, "连接断开")));
            }
            if session_ok == SessionEnd::Shutdown {
                return;
            }
            need_full_reconnect = true;
        }
    }
}

#[derive(PartialEq)]
enum SessionEnd {
    Disconnect,
    Shutdown,
}

async fn run_session(
    reader: &mut BufReader<tokio::io::ReadHalf<tokio::net::windows::named_pipe::NamedPipeClient>>,
    wr: &mut tokio::io::WriteHalf<tokio::net::windows::named_pipe::NamedPipeClient>,
    state: Arc<IpcState>,
    cmd_rx: &mut mpsc::UnboundedReceiver<Cmd>,
    pending: &mut HashMap<u64, Reply>,
    next_id: &mut u64,
    heartbeat: &mut tokio::time::Interval,
) -> SessionEnd {
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                send_line(wr, &(Message::Request(Request{ jsonrpc:"2.0".into(), id:*next_id, method: method::PING.into(), params: None }).to_line())).await;
                *next_id += 1;
            }
            line = next_line(reader) => {
                match line {
                    Some(Ok(l)) => match Message::from_line(&l) {
                        Ok(Message::Response(r)) => {
                            if let Some(rep) = pending.remove(&r.id) {
                                let out = match r.result {
                                    Some(v) => Ok(v),
                                    None => Err(r.error.unwrap_or_else(|| RpcError::new(ERR_INTERNAL_ERROR, "无结果"))),
                                };
                                let _ = rep.send(out);
                            }
                        }
                        Ok(Message::Notification(n)) => { if !dispatch_notification(&state, &n) { return SessionEnd::Shutdown; } }
                        _ => {}
                    },
                    _ => return SessionEnd::Disconnect,
                }
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(Cmd::Request{ method, params, reply }) => {
                        let id = *next_id; *next_id += 1;
                        pending.insert(id, reply);
                        let line = Message::Request(Request{ jsonrpc:"2.0".into(), id, method, params }).to_line();
                        if !send_line(wr, &line).await { return SessionEnd::Disconnect; }
                    }
                    None => return SessionEnd::Shutdown,
                }
            }
        }
    }
}

/// 握手：发送 hello，读取它唯一的响应。返回 HelloResult。
async fn handshake(
    wr: &mut tokio::io::WriteHalf<tokio::net::windows::named_pipe::NamedPipeClient>,
    reader: &mut BufReader<tokio::io::ReadHalf<tokio::net::windows::named_pipe::NamedPipeClient>>,
) -> Option<HelloResult> {
    let req = Request {
        jsonrpc: "2.0".into(),
        id: 1,
        method: method::HELLO.into(),
        params: Some(serde_json::to_value(HelloParams { client_version: CLIENT_VERSION.to_string() }).ok()?),
    };
    if !send_line(wr, &Message::Request(req).to_line()).await {
        return None;
    }
    while let Some(Ok(l)) = next_line(reader).await {
        if let Ok(Message::Response(r)) = Message::from_line(&l) {
            if r.id == 1 {
                if let Some(v) = r.result {
                    return serde_json::from_value::<HelloResult>(v).ok();
                }
            }
        }
    }
    None
}

async fn send_line(
    wr: &mut tokio::io::WriteHalf<tokio::net::windows::named_pipe::NamedPipeClient>,
    line: &str,
) -> bool {
    if wr.write_all(line.as_bytes()).await.is_err() {
        return false;
    }
    wr.write_all(b"\n").await.is_ok()
}

async fn next_line(
    r: &mut BufReader<tokio::io::ReadHalf<tokio::net::windows::named_pipe::NamedPipeClient>>,
) -> Option<std::io::Result<String>> {
    let mut line = String::new();
    match r.read_line(&mut line).await {
        Ok(0) => None,
        Ok(_) => Some(Ok(line)),
        Err(e) => Some(Err(e)),
    }
}

/// 处理服务端事件；返回 false 表示命令通道要求关闭（驱动退出）。
fn dispatch_notification(state: &IpcState, n: &Notification) -> bool {
    match n.method.as_str() {
        event::LOG_APPEND => {
            if let Some(v) = &n.params {
                if let Ok(p) = serde_json::from_value::<LogAppendEvent>(v.clone()) {
                    let _ = state.events.send(IpcEvent::Log { line: p.line });
                }
            }
        }
        event::PROC_METRICS => {
            if let Some(v) = &n.params {
                if let Ok(p) = serde_json::from_value::<ProcMetrics>(v.clone()) {
                    let _ = state.events.send(IpcEvent::Metrics {
                        run_id: p.run_id,
                        cpu_usage: p.cpu_usage,
                        memory_mb: p.memory_mb,
                    });
                }
            }
        }
        event::PROC_STATUS => {
            if let Some(v) = &n.params {
                let run_id = v.get("run_id").and_then(|x| x.as_i64()).unwrap_or(0);
                let status =
                    v.get("status").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let _ = state.events.send(IpcEvent::ProcStatus { run_id, status });
            }
        }
        _ => {}
    }
    true
}

// ---------------------------------------------------------------------------
// daemon 进程管理（拉起 / 存活探测）——同步接口，供命令层调用
// ---------------------------------------------------------------------------

/// 上次尝试拉起 daemon 的时刻（用于断连重拉冷却，避免重试风暴）。
static LAST_SPAWN_AT: std::sync::Mutex<std::option::Option<std::time::Instant>> =
    std::sync::Mutex::new(None);

/// 重拉冷却：两次拉起尝试至少间隔此值。
const RESPAWN_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(2);

/// daemon 是否已在运行（按进程名匹配）。
pub fn is_daemon_alive() -> bool {
    let mut sys = sysinfo::System::new_all();
    sys.refresh_all();
    sys.processes()
        .values()
        .any(|p| p.name().to_string_lossy().to_lowercase().contains("javaboot-daemon"))
}

/// 结束运行中的 javaboot-daemon 进程（升级安装前调用）。
///
/// 仅结束 daemon 自身进程：daemon 持有的 Job **不设 KILL_ON_JOB_CLOSE**，
/// 因此已托管的 java 服务进程不会随 daemon 被连带杀掉。这些服务进程在
/// 新版 daemon 启动后经崩溃恢复（`recover`）重新接管。返回被结束的进程数。
pub fn stop_daemon() -> usize {
    let mut sys = sysinfo::System::new_all();
    sys.refresh_all();
    let mut killed = 0;
    let ids: Vec<sysinfo::Pid> = sys
        .processes()
        .iter()
        .filter(|(_, p)| {
            p.name().to_string_lossy().to_lowercase().contains("javaboot-daemon")
        })
        .map(|(pid, _)| *pid)
        .collect();
    for pid in ids {
        if let Some(p) = sys.process(pid) {
            p.kill();
            killed += 1;
            log::info!("已结束 daemon 进程 pid={pid}");
        }
    }
    killed
}

/// 定位 daemon 可执行文件：优先当前 exe 同级，其次常见安装/数据目录，最后 PATH。
fn locate_daemon_exe() -> Option<std::path::PathBuf> {
    let mut v: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            // 同目录 & 其上两级（target 布局: .../target/debug ↔ daemon 也在 debug）
            v.push(dir.join("javaboot-daemon.exe"));
            v.push(dir.join("resources").join("javaboot-daemon.exe"));
            // bundle.resources 相对路径会原样落到安装目录：target/release/javaboot-daemon.exe
            v.push(dir.join("target").join("release").join("javaboot-daemon.exe"));
            if let Some(gp) = dir.parent().and_then(|p| p.parent()) {
                v.push(gp.join("release").join("javaboot-daemon.exe"));
            }
        }
        v.push(me.with_file_name("javaboot-daemon.exe"));
    }
    // launcher 数据目录（个别安装器把 sidecar 放这里）
    if let Some(data) = dirs::data_dir() {
        v.push(data.join("javaboot-launcher").join("javaboot-daemon.exe"));
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            v.push(dir.join("javaboot-daemon.exe"));
        }
    }
    v.into_iter().find(|p| p.exists())
}

/// 拉起 daemon。返回是否实际执行了拉起（是否「已经在跑」由 daemon 自己判定并退出）。
///
/// 可反复调用：内部带 2s 冷却，故「连接失败自动重拉」不会刷屏；幂等（daemon 单实例）。
pub fn spawn_daemon_process() -> std::io::Result<bool> {
    // 冷却：距上次尝试不足阈值则跳过（防断连重试风暴）
    {
        let mut last = LAST_SPAWN_AT.lock().unwrap();
        if let Some(t) = *last {
            if t.elapsed() < RESPAWN_COOLDOWN {
                return Ok(false);
            }
        }
        *last = Some(std::time::Instant::now());
    }
    let Some(exe) = locate_daemon_exe() else {
        log::warn!("找不到 javaboot-daemon.exe，跳过拉起");
        return Ok(false);
    };
    let mut cmd = std::process::Command::new(&exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NO_WINDOW
        cmd.creation_flags(0x0800_0008_u32);
    }
    match cmd.spawn() {
        Ok(child) => {
            let _ = child.id();
            log::info!("已拉起 daemon: {}", exe.display());
            Ok(true)
        }
        Err(e) => Err(e),
    }
}