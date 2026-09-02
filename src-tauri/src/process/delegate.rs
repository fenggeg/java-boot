//! daemon 委托桥（P4）：把服务启停/重启的生命周期交给常驻 daemon。
//!
//! launcher 仍是"编排者"（编译 / classpath / env / 端口探测等留在本侧），但当
//! daemon 在线时，真正的 java 进程 spawn / 管道消费 / 退出 / 就绪由 daemon 承担。
//!
//! 关键数据结构：`service_id ↔ run_id` 双向映射。daemon 事件（`proc.status` /
//! `proc.metrics` / `log.append`）只带 run_id，本模块把它们归一到 service 维度，
//! 复用 launcher 既有的 `service://status` / `service://log` 事件通道，让现有
//! UI（ServiceCard/日志 Tab）无需感知 daemon 的存在。

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex as PMutex;
use tauri::{AppHandle, Manager};

use jb_core::model::SpawnRequest;
use jb_core::protocol as P;

use crate::error::AppError;
use crate::ipc::{IpcEvent, IpcState};

/// `service_id ↔ run_id` 双向映射的全局单例。
struct DaemonBridge {
    /// service_id -> daemon run_id（正向）
    fwd: PMutex<HashMap<String, i64>>,
    /// run_id -> service_id（反向，事件归一用）
    rev: PMutex<HashMap<i64, String>>,
}

impl DaemonBridge {
    fn new() -> Self {
        DaemonBridge {
            fwd: PMutex::new(HashMap::new()),
            rev: PMutex::new(HashMap::new()),
        }
    }

    fn register(&self, service_id: &str, run_id: i64) {
        let mut fwd = self.fwd.lock();
        if let Some(old) = fwd.insert(service_id.to_string(), run_id) {
            // 覆盖时清理旧 run 的反向映射，避免陈旧指针
            self.rev.lock().remove(&old);
        }
        self.rev.lock().insert(run_id, service_id.to_string());
    }

    /// 取 service 当前 daemon run_id；没有则 None（未走 daemon）
    fn run_of(&self, service_id: &str) -> Option<i64> {
        self.fwd.lock().get(service_id).copied()
    }

    fn service_of(&self, run_id: i64) -> Option<String> {
        self.rev.lock().get(&run_id).cloned()
    }

    fn remove(&self, service_id: &str) {
        if let Some(rid) = self.fwd.lock().remove(service_id) {
            self.rev.lock().remove(&rid);
        }
    }

    fn is_managed(&self, service_id: &str) -> bool {
        self.run_of(service_id).is_some()
    }
}

fn bridge() -> &'static DaemonBridge {
    static BRIDGE: once_cell::sync::Lazy<DaemonBridge> =
        once_cell::sync::Lazy::new(DaemonBridge::new);
    &BRIDGE
}

/// 一次委托给 daemon 启动所需的完整载荷（由 launcher start 侧构造）。
pub struct Launch {
    /// 完整 java 命令（argv[0] = java 可执行）。
    pub argv: Vec<String>,
    /// 显式注入的环境变量（Daemon 环境相对于 launcher 独立，需完整传值）。
    pub env_vars: Vec<(String, String)>,
    pub working_dir: String,
    pub project_id: String,
    pub module_name: String,
    /// 就绪判定应用端口；None 则 daemon 退化为日志正则兜底。
    pub startup_port: Option<u16>,
}

/// 通过 daemon 拉起一个 java 进程（argv[0] = java 可执行，含全部参数）。
/// 返回 (daemon run_id, pid)，并注册 service_id ↔ run_id 映射。
pub async fn spawn_service(
    app: &AppHandle,
    service_id: &str,
    module_name: &str,
    project_id: &str,
    argv: Vec<String>,
    env_vars: Vec<(String, String)>,
    working_dir: String,
    startup_port: Option<u16>,
) -> Result<(i64, u32), AppError> {
    let state = app.state::<Arc<IpcState>>().inner().clone();
    let req = SpawnRequest {
        project_id: project_id.to_string(),
        module_name: module_name.to_string(),
        main_class: None,
        classpath_key: None,
        argv,
        env_vars: env_vars.into_iter().collect(),
        working_dir,
        dev_mode: false,
        auto_restart: false,
        startup_port,
    };
    let params = serde_json::to_value(req).map_err(|e| AppError::Other(format!("序列化失败: {e}")))?;
    let v = state
        .request::<serde_json::Value>(P::method::SPAWN, params)
        .await?;
    let run_id = v.get("run_id").and_then(|x| x.as_i64()).ok_or_else(|| AppError::Other("daemon 未返回 run_id".into()))?;
    let pid = v.get("pid").and_then(|x| x.as_u64()).map(|p| p as u32).unwrap_or(0);
    bridge().register(service_id, run_id);
    Ok((run_id, pid))
}

/// 停止一个由 daemon 托管的服务。返回是否确实委托给了 daemon。
pub async fn stop_service(app: &AppHandle, service_id: &str) -> Result<bool, AppError> {
    let Some(run_id) = bridge().run_of(service_id) else {
        return Ok(false);
    };
    let state = app.state::<Arc<IpcState>>().inner().clone();
    state
        .request::<serde_json::Value>(P::method::STOP, serde_json::json!({ "run_id": run_id }))
        .await?;
    bridge().remove(service_id);
    Ok(true)
}

/// daemon 是否在线（启动委托的前置判断）。
pub fn daemon_online(app: &AppHandle) -> bool {
    app.state::<Arc<IpcState>>().inner().is_connected()
}

/// 该 service 是否已映射到 daemon 托管（决定 stop/restart 是否委托）。
pub fn is_managed(service_id: &str) -> bool {
    bridge().is_managed(service_id)
}

/// 应用重启 / daemon 重连后，重建 `service_id ↔ run_id` 映射。
///
/// 原理：daemon 重启后仍在托管之前的 java 进程（管道、退出、就绪、日志均存活），
/// 仅 launcher 丢失了内存映射。这里用「launcher DB 持久化的 pid」与
/// 「daemon `proc.list` 的 pid」做连接，命中即恢复映射并标记 Running——
/// 此后 daemon 的 `proc.status` / `proc.metrics` / `log.append` 事件会按映射归一，
/// 实时日志**无需重启服务**即可继续推送，也不再触发「管道已断开」的误导提示。
pub async fn rebind(app: &AppHandle) -> crate::error::AppResult<()> {
    if !daemon_online(app) {
        return Ok(());
    }
    let state = app.state::<Arc<IpcState>>().inner().clone();
    let procs = state.reconcile().await?;
    // pid -> (run_id)（仅取 daemon 判定为运行中的进程）
    let live: std::collections::HashMap<u32, i64> = procs
        .iter()
        .filter(|p| p.status == jb_core::model::ProcStatus::Running)
        .filter_map(|p| p.pid.map(|pid| (pid, p.run_id)))
        .collect();
    if live.is_empty() {
        return Ok(());
    }
    let saved = match crate::db::load_all_run_pids() {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    let mgr = crate::process::get_manager();
    for (sid, pid, _started_at) in saved {
        if let Some(run_id) = live.get(&pid) {
            bridge().register(&sid, *run_id);
            mgr.set_status(app, &sid, crate::db::models::ServiceStatus::Running);
            log::info!(
                "daemon 重连恢复映射: service={sid} run_id={run_id} (pid={pid})，实时日志已恢复"
            );
        }
    }
    Ok(())
}

/// 把 daemon 事件归一到 launcher 的 service 维度：
/// 命中映射的服务，驱动 `service://status` / `service://log` 与 runtime 指标。
///
/// 由 launcher setup 订阅 `IpcState.events` 调用；未命中映射的事件直接忽略
/// （这些 run 不属于 launcher 管理的服务，常见于崩溃恢复枚举的存活进程）。
pub fn normalize_event(app: &AppHandle, ev: &IpcEvent) {
    match ev {
        IpcEvent::Disconnected => {
            // daemon 断连：清空全部映射（进程随 daemon 独立存活的实例由恢复流程接管）
            bridge().fwd.lock().clear();
            bridge().rev.lock().clear();
        }
        IpcEvent::ProcStatus { run_id, status } => {
            let Some(sid) = bridge().service_of(*run_id) else { return };
            let mgr = crate::process::get_manager();
            let st = match status.as_str() {
                "running" => crate::db::models::ServiceStatus::Running,
                "starting" => crate::db::models::ServiceStatus::Starting,
                "stopping" => crate::db::models::ServiceStatus::Stopping,
                "stopped" => crate::db::models::ServiceStatus::Stopped,
                "error" => crate::db::models::ServiceStatus::Error,
                _ => return,
            };
            mgr.set_status(app, &sid, st);
        }
        IpcEvent::Metrics { run_id, cpu_usage, memory_mb } => {
            let Some(sid) = bridge().service_of(*run_id) else { return };
            let mgr = crate::process::get_manager();
            mgr.set_metrics(app, &sid, *cpu_usage, *memory_mb);
        }
        IpcEvent::Log { line } => {
            let Some(sid) = bridge().service_of(line.run_id) else { return };
            let tag = if line.stream == jb_core::model::Stream::Stderr { "stderr" } else { "stdout" };
            crate::process::log_pipe::emit_log_raw(app, &sid, tag, &line.body);
        }
        IpcEvent::Connected(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_roundtrip() {
        let b = DaemonBridge::new();
        b.register("svc-1", 42);
        assert_eq!(b.run_of("svc-1"), Some(42));
        assert_eq!(b.service_of(42).as_deref(), Some("svc-1"));
        b.remove("svc-1");
        assert_eq!(b.run_of("svc-1"), None);
        assert_eq!(b.service_of(42), None);
    }

    #[test]
    fn override_and_cleanup() {
        let b = DaemonBridge::new();
        b.register("a", 1);
        b.register("b", 2);
        b.register("a", 3); // 覆盖，旧 run 2? 不对，覆盖 a 与 b 无关
        assert_eq!(b.run_of("a"), Some(3));
        assert_eq!(b.service_of(1), None); // 旧映射失效
        assert_eq!(b.service_of(3).as_deref(), Some("a"));
    }
}