//! 命名管道 + JSON-RPC 2.0 服务端（ADR-0001 决策 1/2）。
//!
//! mask：接受连接 → 会话（split 读写）→ 请求分发 + 事件推送 → 心跳/空闲自杀。

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::net::windows::named_pipe::ServerOptions;
use tokio::sync::{broadcast, mpsc};
use tokio_util::codec::{FramedRead, LinesCodec};

use jb_core::consts as C;
use jb_core::model::SpawnRequest;
use jb_core::protocol::*;

use crate::app::AppState;
use crate::error::Error;

/// 单行 JSON 最大长度（日志正文可能很长）。
const MAX_LINE: usize = 8 * 1024 * 1024;

/// 启动 daemon 主循环（阻塞直到进程被要求退出）。
pub async fn run(state: Arc<AppState>) -> anyhow::Result<()> {
    // 崩溃恢复三态枚举（daemon 启动即执行，随 hello 上报 UI）
    if let Err(e) = state.procs.recover().await {
        log::warn!("启动崩溃恢复枚举失败: {}", e);
    }

    // 空闲自杀 + 定时清理
    spawn_idle_watchdog(Arc::clone(&state));
    spawn_cleanup_loop(Arc::clone(&state));

    let mut first = true;
    loop {
        let mut opts = ServerOptions::new();
        opts.first_pipe_instance(first);
        first = false;
        let server = match opts.create(C::PIPE_NAME) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("创建命名管道失败: {e}，500ms 后重试");
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
        };
        match server.connect().await {
            Ok(()) => {
                *state.last_activity.lock() = std::time::Instant::now();
                let st = Arc::clone(&state);
                tokio::spawn(async move {
                    let _ = handle_session(st, server).await;
                });
            }
            Err(e) => {
                log::debug!("等待连接失败: {e}");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

/// 处理单个连接直到断开。
async fn handle_session(state: Arc<AppState>, pipe: tokio::net::windows::named_pipe::NamedPipeServer) -> anyhow::Result<()> {
    state.sessions.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let (rd, mut wr) = tokio::io::split(pipe);

    // 出站通道：写线程负责把 line 刷到管道。
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if wr.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            if wr.write_all(b"\n").await.is_err() {
                break;
            }
        }
    });

    // 事件订阅器：把服务端事件总线转发为 log.append / proc.status 通知。
    let mut event_rx = state.events.subscribe();
    let ev_tx = tx.clone();
    let ev_writer = tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(msg) => {
                    if ev_tx.send(msg.to_line()).is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    let mut framed = FramedRead::new(rd, LinesCodec::new_with_max_length(MAX_LINE));
    let handshake_done = Arc::new(AtomicBool::new(false));

    while let Some(line) = framed.next().await {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        *state.last_activity.lock() = std::time::Instant::now();
        let msg = match Message::from_line(&line) {
            Ok(m) => m,
            Err(e) => {
                let _ = tx.send(
                    Message::Response(Response::err(0, RpcError::new(ERR_INVALID_REQUEST, e)))
                        .to_line(),
                );
                continue;
            }
        };
        match msg {
            Message::Request(req) => {
                let resp = match route(&state, &req, &handshake_done).await {
                    Ok(v) => Response::ok(req.id, v),
                    Err(e) => Response::err(req.id, e),
                };
                let _ = tx.send(Message::Response(resp).to_line());
            }
            Message::Notification(_) => { /* 客户端单向通知，P1 扩展 */ }
            Message::Response(_) => { /* daemon 是服务端，忽略客户端响应 */ }
        }
    }

    // 连接结束：释放会话计数，停掉写线程与事件订阅。
    state.sessions.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    drop(tx);
    let _ = writer.await;
    ev_writer.abort();
    Ok(())
}

/// 方法路由。
async fn route(
    state: &AppState,
    req: &Request,
    handshake_done: &Arc<AtomicBool>,
) -> std::result::Result<serde_json::Value, RpcError> {
    // 握手门禁：未 hello 只允许 HELLO。
    if req.method != method::HELLO && !handshake_done.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(RpcError::new(ERR_HANDSHAKE_REQUIRED, "请先 daemon.hello"));
    }

    match req.method.as_str() {
        method::HELLO => {
            let _h: HelloParams = parse_params(req)?;
            handshake_done.store(true, std::sync::atomic::Ordering::SeqCst);
            serde_json::to_value(HelloResult {
                daemon_version: C::DAEMON_VERSION.into(),
                min_client_version: C::MIN_CLIENT_VERSION.into(),
                protocol_version: C::PROTOCOL_VERSION,
                has_running: state.procs.has_active(),
                has_pending_recovery: state.procs.has_pending_recovery(),
            })
            .map_err(|e| RpcError::new(ERR_INTERNAL_ERROR, e.to_string()))
        }
        method::PING => Ok(serde_json::json!({ "pong": true })),
        method::SPAWN => {
            let sp: SpawnRequest = parse_params(req)?;
            let hello = HelloResult {
                daemon_version: C::DAEMON_VERSION.into(),
                min_client_version: C::MIN_CLIENT_VERSION.into(),
                protocol_version: C::PROTOCOL_VERSION,
                has_running: false,
                has_pending_recovery: false,
            };
            let (spec, pid) = state.procs.spawn(&sp, &hello).await.map_err(Error::rpc)?;
            serde_json::to_value(spawn_result(spec.run_id, pid))
                .map_err(|e| RpcError::new(ERR_INTERNAL_ERROR, e.to_string()))
        }
        method::STOP => {
            let p: StopParams = parse_params(req)?;
            state.procs.stop(p.run_id).await.map_err(Error::rpc)?;
            Ok(serde_json::json!(null))
        }
        method::LIST => {
            serde_json::to_value(state.procs.list())
                .map_err(|e| RpcError::new(ERR_INTERNAL_ERROR, e.to_string()))
        }
        method::LOG_TAIL => {
            let p: LogTailParams = parse_params(req)?;
            let (next_seq, entries) = state
                .store
                .clone()
                .tail(p.run_id, p.after_seq, p.limit)
                .await
                .map_err(Error::rpc)?;
            serde_json::to_value(LogTailResult { next_seq, entries })
                .map_err(|e| RpcError::new(ERR_INTERNAL_ERROR, e.to_string()))
        }
        method::SPEC_GET => {
            let p: SpecGetParams = parse_params(req)?;
            let spec = state.store.clone().get_spec(p.run_id).await.map_err(Error::rpc)?;
            match spec {
                Some(s) => serde_json::to_value(s)
                    .map_err(|e| RpcError::new(ERR_INTERNAL_ERROR, e.to_string())),
                None => Err(RpcError::new(ERR_INVALID_PARAMS, format!("run {} 无 spec", p.run_id))),
            }
        }
        method::RECOVERY_LIST => {
            serde_json::to_value(RecoveryListResult {
                pending: state.procs.pending_recovery(),
            })
            .map_err(|e| RpcError::new(ERR_INTERNAL_ERROR, e.to_string()))
        }
        method::RECOVERY_RESCAN => {
            // 运行时重新枚举存活 java 并刷新待处置列表（daemon 长驻不自发枚举，
            // 供 launcher 将本地启动的进程引导纳管进 daemon）
            state.procs.recover().await.map_err(Error::rpc)?;
            serde_json::to_value(RecoveryListResult {
                pending: state.procs.pending_recovery(),
            })
            .map_err(|e| RpcError::new(ERR_INTERNAL_ERROR, e.to_string()))
        }
        method::RECOVERY_TAKEOVER => {
            let a: RecoveryAct = parse_params(req)?;
            state.procs.recovery_takeover(a.pid).await.map_err(Error::rpc)?;
            Ok(serde_json::json!(null))
        }
        method::RECOVERY_RESTART => {
            let a: RecoveryAct = parse_params(req)?;
            let rid = state.procs.recovery_restart(a.pid).await.map_err(Error::rpc)?;
            Ok(serde_json::json!({ "run_id": rid }))
        }
        method::RECOVERY_IGNORE => {
            let a: RecoveryAct = parse_params(req)?;
            state.procs.recovery_ignore(a.pid).map_err(Error::rpc)?;
            Ok(serde_json::json!(null))
        }
        method::SCAN_START => {
            let p: ScanStartParams = parse_params(req)?;
            let out = state.scan.start(&p.project_path).await.map_err(Error::rpc)?;
            serde_json::to_value(out)
                .map_err(|e| RpcError::new(ERR_INTERNAL_ERROR, e.to_string()))
        }
        method::SCAN_CANCEL => {
            let p: ScanCancelParams = parse_params(req)?;
            state.scan.cancel(&p.scan_id).map_err(Error::rpc)?;
            Ok(serde_json::json!(null))
        }
        other => Err(RpcError::new(ERR_METHOD_NOT_FOUND, format!("未知方法 {other}"))),
    }
}

/// 空闲自杀：无运行服务 + 无 UI 连接持续 10 分钟 → 退出。
fn spawn_idle_watchdog(state: Arc<AppState>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            if state.procs.has_active() {
                continue;
            }
            if state.sessions.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                continue;
            }
            if state.last_activity.lock().elapsed().as_secs() >= C::IDLE_SHUTDOWN_SECS {
                log::info!("无运行服务且无 UI 连接超时，daemon 自我退出");
                std::process::exit(0);
            }
        }
    });
}

/// 每小时保留策略清理。
fn spawn_cleanup_loop(state: Arc<AppState>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            if let Err(e) = state.store.clone().cleanup(C::LOG_RETENTION_DAYS).await {
                log::warn!("日志保留清理失败: {}", e);
            }
        }
    });
}

fn parse_params<T: serde::de::DeserializeOwned>(
    req: &Request,
) -> std::result::Result<T, RpcError> {
    req.params
        .clone()
        .ok_or_else(|| RpcError::new(ERR_INVALID_PARAMS, "缺少 params"))
        .and_then(|v| {
            serde_json::from_value(v)
                .map_err(|e| RpcError::new(ERR_INVALID_PARAMS, format!("params 解析失败: {e}")))
        })
}

/// proc.spawn 响应载荷。
fn spawn_result(run_id: i64, pid: u32) -> serde_json::Value {
    serde_json::json!({ "run_id": run_id, "pid": pid })
}