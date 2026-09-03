//! 进程服务：spawn / stop / 管道 ingestion / 退出码 / 就绪判定（R5）。
//!
//! 焦点约束（一致性）：
//! - Job Object 由 daemon 持有；UI 崩溃不影响子进程
//! - 日志经 stdout/stderr 管道 → `LogPipeline`
//! - 就绪判定（R5）：主通道为对目标端口做 TCP connect（500ms 间隔，上限 300s），
//!   兜底为 `Started ... in ... seconds` / `APPLICATION FAILED TO START` 日志正则，
//!   任一命中即 running / 判 error；状态沿 Starting → Running|Error 推进并以
//!   `proc.status` 事件通知客户端。

use std::collections::{BTreeMap, HashMap};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use regex::Regex;
use tokio::io::{AsyncBufReadExt, AsyncRead};
use tokio::net::TcpStream;
use tokio::process::Child;
use tokio::sync::broadcast;

use jb_core::consts as C;
use jb_core::model::{
    now_ms, ProcessInfo, ProcessSpec, ProcStatus, RecoveryEntry, RecoveryKind, SpawnRequest,
    Stream,
};
use jb_core::protocol::{event, HelloResult, Message, Notification};
use jb_core::redact::REDACTED;

use crate::error::{Error, Result};
use crate::job::JobObject;
use crate::log_pipe::{self, LogPipeline};
use crate::store::Store;

/// `Started ... in ... seconds` 正则（Spring Boot 就绪兜底）。
static STARTED_RE: once_cell::sync::Lazy<Regex> = once_cell::sync::Lazy::new(|| {
    Regex::new(r"Started\s+\S+\s+in\s+.*seconds").expect("合法正则")
});

/// 启动过程判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Readiness {
    Started,
    Error,
}

/// 对单行启动日志做就绪判定。
///
/// 就绪信号与 launcher `log_pipe::check_started` 对齐（daemon 托管服务只靠日志判定，
/// launcher 本地路径的丰富信号必须在此同样生效，否则服务启动成功也会一直 'starting'）：
/// - 失败：`APPLICATION FAILED TO START` / `BUILD FAILURE`
/// - 成功：`Started xxx in N.NNN seconds`、`Tomcat/Jetty/Netty started on port`、
///   `Undertow started`
fn classify_startup_line(line: &str) -> Option<Readiness> {
    if line.contains("APPLICATION FAILED TO START") || line.contains("BUILD FAILURE") {
        return Some(Readiness::Error);
    }
    let started = STARTED_RE.is_match(line)
        // 宽匹配：与 launcher 一致，限制行长避免日志中段 "Started..." 误命中
        || (line.len() <= 200 && line.contains("Started ") && line.contains(" in ") && line.contains("second"))
        || line.contains("Tomcat started on port")
        || line.contains("Jetty started on port")
        || line.contains("Netty started on port")
        || line.contains("Undertow started");
    if started {
        return Some(Readiness::Started);
    }
    None
}

pub struct ProcHandle {
    run_id: i64,
    project_id: String,
    module_name: String,
    pid_slot: Arc<Mutex<Option<u32>>>,
    seq: Arc<AtomicI64>,
    /// 共享状态：Starting/Running/Error（lifecycle 退出时置 Stopped）。
    status: Arc<Mutex<ProcStatus>>,
    port: Option<u16>,
    started_at: i64,
    /// 子进程所有权：由 run_lifecycle 独占消费。
    child: tokio::sync::Mutex<Option<Child>>,
    /// 崩溃恢复「接管监控」的进程（无管道、无 lifecycle；仅被 daemon 跟踪）。
    adopted: bool,
    /// P3 监控：最近一次采样的 (cpu%, 内存 MB)。由 MonitorService 周期回填。
    metrics: Arc<Mutex<(Option<f32>, Option<f64>)>>,
}

impl ProcHandle {
    fn status(&self) -> ProcStatus {
        self.status.lock().clone()
    }
    fn set_status(&self, s: ProcStatus) {
        *self.status.lock() = s;
    }
    /// 仅当仍处于 Starting 时才推进到 target（避免 Running 被回退/重复判定）。
    fn advance_if_starting(&self, target: ProcStatus) -> bool {
        let mut g = self.status.lock();
        if *g == ProcStatus::Starting {
            *g = target;
            true
        } else {
            false
        }
    }
}

pub struct ProcService {
    store: Arc<Store>,
    log: Arc<LogPipeline>,
    job: Arc<JobObject>,
    /// 服务端事件总线（用于 `proc.status` 通知）。
    bus: broadcast::Sender<Message>,
    runs: Arc<Mutex<HashMap<i64, Arc<ProcHandle>>>>,
    /// 崩溃恢复待处置列表（pid → 条目）。
    recovery: Mutex<Vec<RecoveryEntry>>,
}

impl ProcService {
    pub fn new(
        store: Arc<Store>,
        log: Arc<LogPipeline>,
        job: Arc<JobObject>,
        bus: broadcast::Sender<Message>,
    ) -> Arc<Self> {
        Arc::new(ProcService {
            store,
            log,
            job,
            bus,
            runs: Arc::new(Mutex::new(HashMap::new())),
            recovery: Mutex::new(Vec::new()),
        })
    }

    /// 拉起一个进程并托管。返回 `(spec, pid)`。
    pub async fn spawn(self: &Arc<Self>, req: &SpawnRequest, hello: &HelloResult) -> Result<(ProcessSpec, u32)> {
        if req.argv.is_empty() {
            return Err(Error::Invalid("SpawnRequest.argv 不能为空".into()));
        }
        // 1. 拿 run_id
        let run_id = self.store.clone().insert_run(req.project_id.clone(), req.module_name.clone()).await?;
        let mirror_path = self.log.attach(run_id, &req.working_dir, &req.module_name);

        // 2. 写 process_spec（SQLite + .spec.json 双写）
        let mut spec = ProcessSpec::from_request(req, run_id, hello.daemon_version.clone());
        spec.log_file = mirror_path.to_string_lossy().to_string();
        self.store.clone().insert_spec(spec.clone()).await?;
        self.write_spec_json(&spec, &req.working_dir)?;

        // 3. 构造并 spawn
        let program = &req.argv[0];
        let args = &req.argv[1..];
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .current_dir(&req.working_dir)
            .envs(req.env_vars.iter().map(|(k, v)| (k.clone(), v.clone())))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(false); // daemon 自己管理生命周期，不随 Drop 杀
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW：0x08000000
            cmd.creation_flags(0x0800_0000_u32);
        }
        let child = cmd.spawn().map_err(|e| {
            Error::Other(format!("spawn 失败: {} ({})", req.module_name, e))
        })?;
        let pid = child.id().ok_or_else(|| Error::Other("无法获取子进程 PID".into()))?;

        // 4. 挂入 Job Object
        let job = Arc::clone(&self.job);
        let assign_pid = pid;
        let jr = tokio::task::spawn_blocking(move || job.assign(assign_pid)).await;
        match jr {
            Ok(Ok(())) => {}
            Ok(Err(e)) => log::warn!("AssignProcessToJobObject 失败(pid={pid}): {}", e),
            Err(je) => log::warn!("AssignProcessToJobObject 任务失败(pid={pid}): {je}"),
        }

        // 5. 回写 PID
        self.store.clone().set_run_pid(run_id, pid).await?;

        let status = Arc::new(Mutex::new(ProcStatus::Starting));
        let handle = Arc::new(ProcHandle {
            run_id,
            project_id: req.project_id.clone(),
            module_name: req.module_name.clone(),
            pid_slot: Arc::new(Mutex::new(Some(pid))),
            seq: Arc::new(AtomicI64::new(0)),
            status,
            port: req.startup_port,
            started_at: now_ms(),
            child: tokio::sync::Mutex::new(Some(child)),
            adopted: false,
            metrics: Arc::new(Mutex::new((None, None))),
        });
        self.runs.lock().insert(run_id, Arc::clone(&handle));
        self.notify_status(&handle, ProcStatus::Starting);

        // 6. 生命周期：split 管道 → 两个 reader（含就绪判定）→ 端口探测 → wait → 收尾
        let bus = self.bus.clone();
        self.spawn_lifecycle(handle, run_id, bus);

        Ok((spec, pid))
    }

    /// 当前托管中的进程事实（对账 / proc.list）。
    pub fn list(&self) -> Vec<ProcessInfo> {
        self.runs.lock().values().map(|h| {
            let pid = *h.pid_slot.lock();
            let ports = h.port.map(|p| vec![p]).unwrap_or_default();
            let (cpu, mem) = *h.metrics.lock();
            ProcessInfo {
                run_id: h.run_id,
                module_name: h.module_name.clone(),
                pid,
                status: h.status(),
                started_at: Some(h.started_at),
                ports: ports.clone(),
                service_ports: ports,
                cpu_usage: cpu,
                memory_mb: mem,
                recovery_hint: None,
            }
        }).collect()
    }

    /// 暴露可被 MonitorService 周期回填的资源采样槽位。
    /// 返回 `(run_id, pid, metrics)`，metrics 由监控线程持有后写回。
    pub fn metrics_slots(
        &self,
    ) -> Vec<(i64, Option<u32>, Arc<Mutex<(Option<f32>, Option<f64>)>>)> {
        self.runs.lock().values().map(|h| {
            (
                h.run_id,
                *h.pid_slot.lock(),
                Arc::clone(&h.metrics),
            )
        }).collect()
    }

    pub fn has_active(&self) -> bool {
        !self.runs.lock().is_empty()
    }

    // ==================== 崩溃恢复（R3） ====================

    /// daemon 启动时枚举存活 java 进程，按三态分类入库待处置列表。
    pub async fn recover(self: &Arc<Self>) -> Result<()> {
        let runs = self.store.clone().list_run_pids().await?;
        let specs = self.store.clone().list_specs_all().await?;
        let java = tokio::task::spawn_blocking(scan_java_procs).await
            .map_err(|e| Error::Other(format!("枚举 java 进程失败: {e}")))?;

        let mut pending: Vec<RecoveryEntry> = Vec::new();
        for (pid, cmd, name) in java {
            let exact = runs
                .iter()
                .find(|(_, p)| p.map_or(false, |p| p == pid))
                .and_then(|(rid, _)| specs.iter().find(|s| s.run_id == *rid))
                .cloned();

            if let Some(spec) = exact {
                pending.push(RecoveryEntry {
                    pid,
                    kind: RecoveryKind::Exact,
                    run_id: Some(spec.run_id),
                    module_name: spec.module_name.clone(),
                    cmdline: cmd.clone(),
                    had_spec: true,
                    startup_port: spec.startup_port,
                });
            } else {
                let kind = if specs.iter().any(|s| cmd_contains(&cmd, s)) {
                    RecoveryKind::Fuzzy
                } else {
                    RecoveryKind::Unknown
                };
                pending.push(RecoveryEntry {
                    pid,
                    kind,
                    run_id: None,
                    module_name: name,
                    cmdline: cmd,
                    had_spec: false,
                    startup_port: None,
                });
            }
        }
        *self.recovery.lock() = pending;
        Ok(())
    }

    pub fn pending_recovery(&self) -> Vec<RecoveryEntry> {
        self.recovery.lock().clone()
    }

    pub fn has_pending_recovery(&self) -> bool {
        !self.recovery.lock().is_empty()
    }

    /// 接管监控：把某存活进程纳入 daemon 跟踪（recovery 处置之一）。
    pub async fn recovery_takeover(self: &Arc<Self>, pid: u32) -> Result<()> {
        let entry = self.take_recovery(pid)?;
        // 优雅：已可能因进程退出而失效
        let (run_id, module, env_ok) = match entry.run_id {
            Some(rid) => {
                let spec = self.store.clone().get_spec(rid).await?;
                match spec {
                    Some(s) => (rid, s.module_name.clone(), true),
                    None => {
                        let rid = self.store.clone().insert_run("recovered".into(), entry.module_name.clone()).await?;
                        (rid, entry.module_name.clone(), false)
                    }
                }
            }
            None => {
                let rid = self.store.clone().insert_run("recovered".into(), entry.module_name.clone()).await?;
                (rid, entry.module_name.clone(), false)
            }
        };
        let _ = env_ok;
        self.store.clone().set_run_pid(run_id, pid).await?;
        let handle = Arc::new(ProcHandle {
            run_id,
            project_id: "recovered".into(),
            module_name: module,
            pid_slot: Arc::new(Mutex::new(Some(pid))),
            seq: Arc::new(AtomicI64::new(0)),
            status: Arc::new(Mutex::new(ProcStatus::Running)),
            port: entry.startup_port,
            started_at: now_ms(),
            child: tokio::sync::Mutex::new(None),
            adopted: true,
            metrics: Arc::new(Mutex::new((None, None))),
        });
        self.runs.lock().insert(run_id, Arc::clone(&handle));
        self.notify_status(&handle, ProcStatus::Running);
        log::info!("崩溃恢复：接管进程 pid={pid} run_id={run_id}");
        Ok(())
    }

    /// 干净重启：用原 spec 以新 run_id 重新拉起（日志归属新 run，续传靠 append）。
    pub async fn recovery_restart(self: &Arc<Self>, pid: u32) -> Result<i64> {
        let entry = self.take_recovery(pid)?;
        let run_id = entry
            .run_id
            .ok_or_else(|| Error::NotFound(format!("pid {pid} 无精确 spec，无法干净重启")))?;
        let spec = self
            .store
            .clone()
            .get_spec(run_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("run {run_id} spec 不存在")))?;
        let req = spec_to_request(&spec)?;
        let hello = self.bare_hello();
        let (new_spec, new_pid) = self.spawn(&req, &hello).await?;
        // 原进程不再跟踪（交给用户决定是否终止；列表已移除，UI 可 stop 新 run）
        log::info!("崩溃恢复：干净重启 pid={pid} → 新 run_id={} pid={new_pid}", new_spec.run_id);
        Ok(new_spec.run_id)
    }

    /// 忽略：从待处置列表移除，不做任何事。
    pub fn recovery_ignore(&self, pid: u32) -> Result<()> {
        std::mem::drop(self.take_recovery(pid)?);
        Ok(())
    }

    /// 取走 pending 中对应 pid 的条目（同时移除）。
    fn take_recovery(&self, pid: u32) -> Result<RecoveryEntry> {
        let mut g = self.recovery.lock();
        let idx = g
            .iter()
            .position(|e| e.pid == pid)
            .ok_or_else(|| Error::NotFound(format!("待处置 pid={pid} 不存在（可能已处理）")))?;
        Ok(g.remove(idx))
    }

    fn bare_hello(&self) -> HelloResult {
        HelloResult {
            daemon_version: C::DAEMON_VERSION.into(),
            min_client_version: C::MIN_CLIENT_VERSION.into(),
            protocol_version: C::PROTOCOL_VERSION,
            has_running: self.has_active(),
            has_pending_recovery: self.has_pending_recovery(),
        }
    }

    fn notify_status(&self, handle: &Arc<ProcHandle>, s: ProcStatus) {
        let status_str = serde_json::to_value(s).and_then(|v| serde_json::from_value::<String>(v)).unwrap_or_default();
        let notif = Notification::raw(event::PROC_STATUS, Some(serde_json::json!({
            "run_id": handle.run_id, "status": status_str,
        })));
        let _ = self.bus.send(Message::Notification(notif));
    }

    // ---------- 停止 ----------

    /// 优雅停止：经 Job/OS 终止 PID，等待真退出 + 端口释放检查。
    pub async fn stop(self: &Arc<Self>, run_id: i64) -> Result<()> {
        let handle = self
            .runs
            .lock()
            .get(&run_id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("run {run_id} 不存在或已停止")))?;
        let pid: Option<u32> = *handle.pid_slot.lock();
        if let Some(pid) = pid {
            let job = Arc::clone(&self.job);
            let jr = tokio::task::spawn_blocking(move || job.terminate_pid(pid)).await;
            match jr {
                Ok(Ok(())) => {}
                Ok(Err(e)) => log::warn!("terminate_pid({pid}) 失败: {}", e),
                Err(je) => log::warn!("terminate_pid 任务失败: {je}"),
            }
        }
        // 接管进程没有 lifecycle：直接清退。
        if handle.adopted {
            handle.pid_slot.lock().take();
            let run_id2 = handle.run_id;
            runs_remove(&self.runs, run_id2);
            return Ok(());
        }
        // 等待 lifecycle 收尾（pid_slot 清空）
        let deadline = Instant::now() + Duration::from_secs(C::STOP_WAIT_PID_SECS);
        loop {
            if handle.pid_slot.lock().is_none() {
                break;
            }
            if Instant::now() >= deadline {
                log::warn!("run {run_id} 停止超时");
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // 端口释放检查（可选：进程已退出但端口可能仍被占用）
        if let Some(port) = handle.port {
            self.check_port_released(port).await;
        }
        Ok(())
    }

    /// 端口释放检查：进程退出后再探测片刻，仍未释放则告警。
    async fn check_port_released(&self, port: u16) {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        for _ in 0..20 {
            match tokio::time::timeout(Duration::from_millis(300), TcpStream::connect(addr)).await {
                Ok(Ok(_)) => {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
                _ => {
                    log::info!("端口 {port} 已释放");
                    return;
                }
            }
        }
        log::warn!("端口 {port} 在进程退出后仍被占用，可能未完全释放");
    }

    // ---------- 生命周期 ----------

    fn spawn_lifecycle(
        self: &Arc<Self>,
        handle: Arc<ProcHandle>,
        run_id: i64,
        bus: broadcast::Sender<Message>,
    ) {
        let store = Arc::clone(&self.store);
        let runs = Arc::clone(&self.runs);
        let log = Arc::clone(&self.log);
        let job = Arc::clone(&self.job);
        let status_handle = Arc::clone(&handle.status);
        let started = Arc::clone(&handle.status);

        // 端口就绪探测（主通道）
        if let Some(port) = handle.port {
            spawn_port_probe(handle.clone(), port, bus.clone());
        }

        tokio::spawn(async move {
            let child = {
                let mut slot = handle.child.lock().await;
                slot.take()
            };
            let Some(mut child) = child else {
                log::warn!("run {run_id} 生命周期：子进程缺失");
                let _ = store.clone().finish_run(run_id, -1).await;
                *status_handle.lock() = ProcStatus::Stopped;
                runs.lock().remove(&run_id);
                return;
            };
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();

            if let Some(so) = stdout {
                stdin_split_reader(Arc::clone(&log), handle.seq.clone(), Arc::clone(&started), bus.clone(), run_id, so, Stream::Stdout, Arc::clone(&job), handle.pid_slot.clone());
            }
            if let Some(se) = stderr {
                stdin_split_reader(Arc::clone(&log), handle.seq.clone(), Arc::clone(&started), bus.clone(), run_id, se, Stream::Stderr, Arc::clone(&job), handle.pid_slot.clone());
            }

            // 等待真退出
            let status = child.wait().await;
            let code = match &status {
                Ok(s) => s.code().unwrap_or(-1),
                Err(_) => -1,
            };
            let _ = store.clone().finish_run(run_id, code).await;
            log::info!("run {run_id} 已退出，code={code}");
            *handle.status.lock() = ProcStatus::Stopped;
            let _ = bus.send(Message::Notification(Notification::raw(
                event::PROC_STATUS,
                Some(serde_json::json!({ "run_id": handle.run_id, "status": "stopped" })),
            )));
            handle.pid_slot.lock().take();
            runs.lock().remove(&run_id);
        });
    }

    fn write_spec_json(&self, spec: &ProcessSpec, working_dir: &str) -> Result<()> {
        let path = PathBuf::from(working_dir)
            .join(".javaboot")
            .join(format!(".spec-{}.json", spec.run_id));
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(Error::Io)?;
        }
        let json = serde_json::to_string_pretty(spec).map_err(Error::Json)?;
        std::fs::write(&path, json).map_err(Error::Io)
    }
}

/// 端口探测：500ms 间隔 TCP connect，成功即 Running；上限 300s。
fn spawn_port_probe(handle: Arc<ProcHandle>, port: u16, bus: broadcast::Sender<Message>) {
    tokio::spawn(async move {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let deadline = Instant::now() + Duration::from_secs(C::READY_TIMEOUT_SECS);
        loop {
            if handle.status() != ProcStatus::Starting {
                return; // 已被日志判定推进（Running/Error）或已退出
            }
            if Instant::now() >= deadline {
                log::warn!("run {} 端口 {port} 就绪探测超时", handle.run_id);
                return;
            }
            match tokio::time::timeout(
                Duration::from_millis(C::PORT_PROBE_INTERVAL_MS),
                TcpStream::connect(addr),
            )
            .await
            {
                Ok(Ok(_)) => {
                    if handle.advance_if_starting(ProcStatus::Running) {
                        log::info!("run {} 就绪（端口 {port} 可达）", handle.run_id);
                        let _ = bus.send(Message::Notification(Notification::raw(
                            event::PROC_STATUS,
                            Some(serde_json::json!({
                                "run_id": handle.run_id, "status": "running"
                            })),
                        )));
                    }
                    return;
                }
                _ => {
                    tokio::time::sleep(Duration::from_millis(C::PORT_PROBE_INTERVAL_MS)).await;
                }
            }
        }
    });
}

/// 逐行读取某管道的输出：送日志管线 + 就绪正则判定。
///
/// `job`/`pid_slot` 用于「启动失败(Error)自动回收」：一旦日志判到启动失败，
/// 立即终止该进程并从托管移除，避免出错实例持续占用端口成为孤儿。
fn stdin_split_reader<R>(
    log: Arc<LogPipeline>,
    seq: Arc<AtomicI64>,
    status: Arc<Mutex<ProcStatus>>,
    bus: broadcast::Sender<Message>,
    run_id: i64,
    pipe: R,
    stream: Stream,
    job: Arc<JobObject>,
    pid_slot: Arc<parking_lot::Mutex<Option<u32>>>,
)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut reader = tokio::io::BufReader::new(pipe).lines();
        loop {
            match reader.next_line().await {
                Ok(Some(line)) => {
                    let s = seq.fetch_add(1, Ordering::SeqCst) + 1;
                    let _ = log.tx.send(log_pipe::make_line(run_id, s, now_ms(), stream, line.clone()));

                    // 就绪判定（兜底正则）
                    if let Some(rd) = classify_startup_line(&line) {
                        let is_fail = matches!(rd, Readiness::Error);
                        if *status.lock() == ProcStatus::Starting {
                            *status.lock() = if is_fail { ProcStatus::Error } else { ProcStatus::Running };
                            let st_str = if is_fail { "error" } else { "running" };
                            log::info!("run {run_id} 日志判定 → {st_str}");
                            let st = serde_json::json!({ "run_id": run_id, "status": st_str });
                            let _ = bus.send(Message::Notification(Notification::raw(event::PROC_STATUS, Some(st))));

                            // 启动失败：终止该进程，释放端口，避免孤儿实例继续占用
                            if is_fail {
                                if let Some(pid) = *pid_slot.lock() {
                                    let job = job.clone();
                                    tokio::task::spawn_blocking(move || {
                                        if let Err(e) = job.terminate_pid(pid) {
                                            log::warn!("run {run_id} 启动失败后终止 pid {pid} 失败: {e}");
                                        } else {
                                            log::info!("run {run_id} 启动失败，已终止 pid {pid}");
                                        }
                                    });
                                }
                            }
                        }
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    log::debug!("run {run_id} 管道读取错误: {}", e);
                    break;
                }
            }
        }
    });
}

// ==================== R3 恢复辅助（自由函数） ====================

/// 从 runs 表中移除某 run。
fn runs_remove(runs: &Arc<Mutex<HashMap<i64, Arc<ProcHandle>>>>, run_id: i64) {
    runs.lock().remove(&run_id);
}

/// 扫描存活 Java 进程，返回 (pid, 命令行摘要, 进程名)。
fn scan_java_procs() -> Vec<(u32, String, String)> {
    let mut sys = sysinfo::System::new_all();
    sys.refresh_all();
    let mut out = Vec::new();
    for (pid, proc) in sys.processes() {
        let name = proc.name().to_string_lossy().to_lowercase();
        if !(name.contains("java") || name.contains("javaw")) {
            continue;
        }
        let cmd = proc
            .cmd()
            .iter()
            .map(|c| c.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        out.push((pid.as_u32(), cmd, proc.name().to_string_lossy().into_owned()));
    }
    out
}

/// 模糊匹配：命令行是否含 spec 的模块名 / 主类特征。
fn cmd_contains(cmd: &str, spec: &ProcessSpec) -> bool {
    let lower = cmd.to_ascii_lowercase();
    if !spec.module_name.is_empty() && lower.contains(&spec.module_name.to_ascii_lowercase()) {
        return true;
    }
    if let Some(mc) = &spec.main_class {
        if !mc.is_empty() && lower.contains(&mc.to_ascii_lowercase()) {
            return true;
        }
    }
    false
}

/// 由 spec 反构造 spawn 请求（env 已脱敏者剔除，无法回填空密；文档化限制）。
fn spec_to_request(spec: &ProcessSpec) -> Result<SpawnRequest> {
    let raw: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&spec.env_vars).unwrap_or_default();
    let env: BTreeMap<String, String> = raw
        .into_iter()
        .filter(|(_, v)| v.as_str().map_or(true, |s| s != REDACTED))
        .map(|(k, v)| (k, v.as_str().unwrap_or_default().to_string()))
        .collect();
    Ok(SpawnRequest {
        project_id: spec.project_id.clone(),
        module_name: spec.module_name.clone(),
        main_class: spec.main_class.clone(),
        classpath_key: spec.classpath_key.clone(),
        argv: spec.argv(),
        env_vars: env,
        working_dir: spec.working_dir.clone(),
        dev_mode: spec.dev_mode,
        auto_restart: spec.auto_restart,
        startup_port: spec.startup_port,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn started_regex_hits_spring_boot_line() {
        let line = "2026-09-02 10:00:00.123  INFO 12345 --- [main] com.demo.Application : Started DemoApplication in 5.432 seconds (process running for 6.1)";
        assert_eq!(classify_startup_line(line), Some(Readiness::Started));
    }

    #[test]
    fn fail_marker_detected() {
        assert_eq!(
            classify_startup_line("APPLICATION FAILED TO START"),
            Some(Readiness::Error)
        );
        assert_eq!(classify_startup_line("BUILD FAILURE"), Some(Readiness::Error));
    }

    #[test]
    fn started_regex_variants() {
        // 宽匹配：不需要 (process running ...) 尾巴；多空格/大小写宽松
        assert_eq!(
            classify_startup_line("Started DemoApplication in 5.432 seconds"),
            Some(Readiness::Started)
        );
        assert_eq!(
            classify_startup_line("   Started   gateway-service   in 12.0  seconds   "),
            Some(Readiness::Started)
        );
        // 非 Spring 的「Started ... in」不应误报就绪（如自定义 banner 里恰好带 Started/in）
        assert_eq!(classify_startup_line("Started in-memory broker"), None);
    }

    #[test]
    fn failure_outranks_started_when_both_present() {
        // 同一行带 Started 又带 FAILED 时必须判 Error（应用可能启动后又失败）
        let line = "Started DemoApplication in 5s ... APPLICATION FAILED TO START";
        assert_eq!(classify_startup_line(line), Some(Readiness::Error));
    }

    #[test]
    fn server_lines_detected_as_started() {
        // daemon 就绪判定与 launcher log_pipe::check_started 对齐：
        // 只认出 "Started ... in ... seconds" 会被部分 web 服务漏判 → 一直 starting。
        assert_eq!(
            classify_startup_line("2026-09-03 ... Tomcat started on port(s): 9090 (http) with context path ''"),
            Some(Readiness::Started)
        );
        assert_eq!(
            classify_startup_line("Netty started on port(s): 9090"),
            Some(Readiness::Started)
        );
        assert_eq!(
            classify_startup_line("Undertow started on port(s) 9090"),
            Some(Readiness::Started)
        );
        assert_eq!(
            classify_startup_line("   Started   gateway-service   in 12.0  seconds   "),
            Some(Readiness::Started)
        );
    }

    #[test]
    fn non_startup_noise_not_started() {
        // 日志中段与 "Started ..." 无关的噪音不应误判就绪
        assert_eq!(classify_startup_line("Started in-memory broker"), None);
        assert_eq!(classify_startup_line("  .   ____          _            __ _ _"), None);
        assert_eq!(classify_startup_line("Tomcat started on port 8080"), Some(Readiness::Started));
    }

    #[test]
    fn advance_only_from_starting_no_rollback() {
        let status = Arc::new(Mutex::new(ProcStatus::Starting));
        let h = ProcHandleRef(status);
        // 仅 Starting 可推进
        assert!(advance_if_starting_owned(&h, ProcStatus::Running));
        assert_eq!(*h.0.lock(), ProcStatus::Running);
        // 已 Running 后，晚到的 Error 判定不得回退/覆盖（读决策8「任一命中即 running」）
        assert!(!advance_if_starting_owned(&h, ProcStatus::Error));
        assert_eq!(*h.0.lock(), ProcStatus::Running);
    }

    /// 辅助测试：包装 Mutexed Status 为只读引用。
    struct ProcHandleRef(Arc<Mutex<ProcStatus>>);
    impl ProcHandleRef {
        fn status(&self) -> ProcStatus {
            self.0.lock().clone()
        }
        fn advance_if_starting(&self, target: ProcStatus) -> bool {
            let mut g = self.0.lock();
            if *g == ProcStatus::Starting {
                *g = target;
                true
            } else {
                false
            }
        }
    }
    fn advance_if_starting_owned(h: &ProcHandleRef, s: ProcStatus) -> bool {
        h.advance_if_starting(s)
    }

    fn noise_lines_ignored() {
        assert_eq!(classify_startup_line("Tomcat started on port 8080"), None);
        assert_eq!(classify_startup_line("  .   ____          _            __ _ _"), None);
    }

    #[test]
    fn cmd_fuzzy_matches_module_name() {
        let spec = ProcessSpec {
            run_id: 1,
            project_id: "p".into(),
            module_name: "user-service".into(),
            main_class: Some("com.demo.UserApplication".into()),
            classpath_key: None,
            jvm_args: r#"["java","-cp","."]"#.into(),
            env_vars: "{}".into(),
            working_dir: r"C:\work".into(),
            dev_mode: false,
            auto_restart: false,
            log_file: String::new(),
            launcher_version: String::new(),
            startup_port: None,
            created_at: 0,
        };
        assert!(cmd_contains(r"C:\jdks\17\bin\java.exe -cp out user-service.App", &spec));
        assert!(!cmd_contains("javaw -jar other.jar", &spec));
    }

    #[test]
    fn spec_to_request_drops_redacted_env() {
        let spec = ProcessSpec {
            run_id: 1,
            project_id: "p".into(),
            module_name: "m".into(),
            main_class: None,
            classpath_key: None,
            jvm_args: r#"["java","-jar","app.jar"]"#.into(),
            env_vars: r#"{"DB_PASSWORD":"«redacted»","PORT":"8080"}"#.into(),
            working_dir: r"C:\work".into(),
            dev_mode: false,
            auto_restart: false,
            log_file: String::new(),
            launcher_version: String::new(),
            startup_port: Some(8080),
            created_at: 0,
        };
        let req = spec_to_request(&spec).unwrap();
        assert_eq!(req.env_vars.get("DB_PASSWORD"), None);
        assert_eq!(req.env_vars.get("PORT").map(|s| s.as_str()), Some("8080"));
        assert_eq!(req.argv, vec!["java", "-jar", "app.jar"]);
        assert_eq!(req.startup_port, Some(8080));
    }
}