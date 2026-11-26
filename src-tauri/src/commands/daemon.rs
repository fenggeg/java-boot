//! Daemon（IPC 代理）：握手 / 对账 / 启停 / 日志 / 崩溃恢复 / 扫描。
use crate::error::{AppError, AppResult};
use crate::ipc;

// ============================ Daemon（IPC 代理） ============================
// P0：暴露 daemon 转发命令，UI 据此成为无状态视图。前端 `src/ipc/` 封装。

/// daemon 是否已连接。
#[tauri::command]
pub fn daemon_connected(state: tauri::State<'_, std::sync::Arc<ipc::IpcState>>) -> bool {
    state.is_connected()
}

/// 握手信息（前端据此做版本协商 & 触发对账）。
#[tauri::command]
pub fn daemon_hello(
    state: tauri::State<'_, std::sync::Arc<ipc::IpcState>>,
) -> Option<jb_core::protocol::HelloResult> {
    state.hello.borrow().clone()
}

/// 对账：拉取 daemon 全部托管进程事实。
#[tauri::command]
pub async fn daemon_reconcile(
    state: tauri::State<'_, std::sync::Arc<ipc::IpcState>>,
) -> AppResult<Vec<jb_core::model::ProcessInfo>> {
    state.reconcile().await
}

/// 拉起 daemon 二进制（若未在跑），供 UI 启动时兜底。
#[tauri::command]
pub fn daemon_ensure_started() -> bool {
    if ipc::is_daemon_alive() {
        return true;
    }
    ipc::spawn_daemon_process().unwrap_or(false)
}

/// 通过 daemon 启动一个进程（P0 验收用）。
#[tauri::command]
pub async fn daemon_spawn(
    state: tauri::State<'_, std::sync::Arc<ipc::IpcState>>,
    req: jb_core::model::SpawnRequest,
) -> AppResult<jb_core::protocol::SpawnResult> {
    let params =
        serde_json::to_value(req).map_err(|e| AppError::Other(format!("入参序列化失败: {e}")))?;
    let v = state
        .request::<serde_json::Value>(jb_core::protocol::method::SPAWN, params)
        .await?;
    serde_json::from_value(v)
        .map_err(|e| AppError::Other(format!("spawn 响应解析失败: {e}")))
}

/// 停止 daemon 托管的进程。
#[tauri::command]
pub async fn daemon_stop(
    state: tauri::State<'_, std::sync::Arc<ipc::IpcState>>,
    run_id: i64,
) -> AppResult<()> {
    state
        .request::<serde_json::Value>(
            jb_core::protocol::method::STOP,
            serde_json::json!({ "run_id": run_id }),
        )
        .await?;
    Ok(())
}

/// 增量拉取运行日志。
#[tauri::command]
pub async fn daemon_logtail(
    state: tauri::State<'_, std::sync::Arc<ipc::IpcState>>,
    run_id: i64,
    after_seq: i64,
) -> AppResult<jb_core::protocol::LogTailResult> {
    let params = serde_json::to_value(jb_core::protocol::LogTailParams {
        run_id,
        after_seq,
        limit: 2000,
    })
    .map_err(|e| AppError::Other(format!("入参序列化失败: {e}")))?;
    state
        .request::<jb_core::protocol::LogTailResult>(jb_core::protocol::method::LOG_TAIL, params)
        .await
}

/// 崩溃恢复：列出 daemon 发现的待处置存活进程（三态）。
#[tauri::command]
pub async fn daemon_recovery_list(
    state: tauri::State<'_, std::sync::Arc<ipc::IpcState>>,
) -> AppResult<Vec<jb_core::model::RecoveryEntry>> {
    state
        .request::<Vec<jb_core::model::RecoveryEntry>>(
            jb_core::protocol::method::RECOVERY_LIST,
            serde_json::json!({}),
        )
        .await
}

/// 崩溃恢复：接管监控某存活进程。
#[tauri::command]
pub async fn daemon_recovery_takeover(
    state: tauri::State<'_, std::sync::Arc<ipc::IpcState>>,
    pid: u32,
) -> AppResult<()> {
    state
        .request::<serde_json::Value>(
            jb_core::protocol::method::RECOVERY_TAKEOVER,
            serde_json::json!({ "pid": pid }),
        )
        .await?;
    Ok(())
}

/// 崩溃恢复：干净重启（用原 spec，新 run_id，日志续传）。
#[tauri::command]
pub async fn daemon_recovery_restart(
    state: tauri::State<'_, std::sync::Arc<ipc::IpcState>>,
    pid: u32,
) -> AppResult<i64> {
    let v = state
        .request::<serde_json::Value>(
            jb_core::protocol::method::RECOVERY_RESTART,
            serde_json::json!({ "pid": pid }),
        )
        .await?;
    v.get("run_id")
        .and_then(|x| x.as_i64())
        .ok_or_else(|| AppError::Other("响应缺少 run_id".into()))
}

/// 崩溃恢复：忽略某存活进程。
#[tauri::command]
pub async fn daemon_recovery_ignore(
    state: tauri::State<'_, std::sync::Arc<ipc::IpcState>>,
    pid: u32,
) -> AppResult<()> {
    state
        .request::<serde_json::Value>(
            jb_core::protocol::method::RECOVERY_IGNORE,
            serde_json::json!({ "pid": pid }),
        )
        .await?;
    Ok(())
}

/// 发起 pom 扫描（命中缓存秒级返回；否则后台 + 进度/完成事件）。
#[tauri::command]
pub async fn daemon_scan_start(
    state: tauri::State<'_, std::sync::Arc<ipc::IpcState>>,
    project_path: String,
) -> AppResult<jb_core::protocol::ScanStartResult> {
    state
        .request::<jb_core::protocol::ScanStartResult>(
            jb_core::protocol::method::SCAN_START,
            serde_json::json!({ "project_path": project_path }),
        )
        .await
}

/// 取消一次扫描。
#[tauri::command]
pub async fn daemon_scan_cancel(
    state: tauri::State<'_, std::sync::Arc<ipc::IpcState>>,
    scan_id: String,
) -> AppResult<()> {
    state
        .request::<serde_json::Value>(
            jb_core::protocol::method::SCAN_CANCEL,
            serde_json::json!({ "scan_id": scan_id }),
        )
        .await?;
    Ok(())
}

