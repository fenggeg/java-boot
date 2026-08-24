use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;
use tauri::AppHandle;

use crate::db::models::Service;
use crate::db::models::ServiceStatus;
use crate::process;

const IGNORE_DIRS: &[&str] = &[
    "target", ".idea", "node_modules", ".git", ".vscode",
    "dist", "build", "out",
];
const IGNORE_EXTS: &[&str] = &["class"];

/// 每个服务的防抖 worker：单线程消费事件，避免每次事件都 spawn 新线程
struct WatchState {
    _watcher: RecommendedWatcher,
    /// 显式取消信号：unwatch 时置 true，worker 线程轮询检测后退出
    _cancel: Arc<AtomicBool>,
    /// worker 线程 handle，unwatch 时 join 确保线程退出
    _worker: Option<std::thread::JoinHandle<()>>,
}

pub struct WatchManager {
    watchers: Mutex<HashMap<String, WatchState>>,
}

impl WatchManager {
    pub fn new() -> Self {
        Self {
            watchers: Mutex::new(HashMap::new()),
        }
    }

    /// 为服务注册文件监听
    pub fn watch(&self, app: AppHandle, service: Service) -> crate::error::AppResult<()> {
        let sid = service.id.clone();
        let sid_for_map = sid.clone();
        // 已存在则先移除（会 drop 旧 watcher + signal cancel + join worker）
        self.unwatch(&sid);

        let cfg = crate::db::load_config().unwrap_or_default();
        let debounce = cfg.auto_restart_debounce_secs;
        let src_main = PathBuf::from(&service.working_dir).join("src").join("main");
        if !src_main.exists() {
            return Err(crate::error::AppError::Other(format!(
                "源码目录不存在: {}",
                src_main.display()
            )));
        }

        // 每服务一个信号 channel：事件只做 try_send，worker 做防抖
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let cancel = Arc::new(AtomicBool::new(false));

        // 启动防抖 worker（单线程），通过 cancel 信号显式控制生命周期
        let worker = {
            let sid_worker = sid.clone();
            let app_worker = app.clone();
            let cancel_worker = cancel.clone();
            std::thread::spawn(move || {
                // 用 catch_unwind 兜底，防止 panic 拖垮整个进程
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let debounce_dur = Duration::from_secs(debounce);
                    // 阻塞等待首个事件
                    while !cancel_worker.load(Ordering::Relaxed) {
                        match rx.recv_timeout(Duration::from_millis(500)) {
                            Ok(()) => {
                                // 收到事件后进入防抖循环：直到 debounce 时间内无新事件才触发
                                loop {
                                    if cancel_worker.load(Ordering::Relaxed) {
                                        return;
                                    }
                                    match rx.recv_timeout(debounce_dur) {
                                        Ok(_) => continue,
                                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                                    }
                                }
                                trigger_restart(&app_worker, &sid_worker);
                            }
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                        }
                    }
                }));
                if let Err(e) = result {
                    log::error!("watch worker 异常退出 ({}): {:?}", sid_worker, e);
                }
            })
        };

        let tx_for_cb = tx.clone();
        let mut watcher = RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                if let Ok(event) = res {
                    if !is_relevant_event(&event) {
                        return;
                    }
                    // 只发信号，不 spawn 线程；channel 满或已关闭则忽略
                    let _ = tx_for_cb.send(());
                }
            },
            Config::default(),
        )
        .map_err(|e| crate::error::AppError::Other(format!("watcher 创建失败: {}", e)))?;

        watcher
            .watch(&src_main, RecursiveMode::Recursive)
            .map_err(|e| crate::error::AppError::Other(format!("监听失败: {}", e)))?;

        self.watchers.lock().insert(
            sid_for_map,
            WatchState {
                _watcher: watcher,
                _cancel: cancel,
                _worker: Some(worker),
            },
        );
        // tx 在此 drop，但 watcher 回调持有 tx_for_cb（克隆），channel 保持 open。
        // unwatch 时 drop WatchState → drop watcher → channel 关闭 + cancel 置 true → worker 退出。
        let _ = tx;
        Ok(())
    }

    pub fn unwatch(&self, service_id: &str) {
        let state = self.watchers.lock().remove(service_id);
        if let Some(mut s) = state {
            // 先 signal cancel，让 worker 尽快退出
            s._cancel.store(true, Ordering::Relaxed);
            // drop watcher 关闭 channel（worker 的 recv 会返回 Disconnected）
            // join worker 确保线程退出，避免泄漏
            if let Some(handle) = s._worker.take() {
                let _ = handle.join();
            }
        }
    }

    /// 根据 db 中 auto_restart 字段，启动所有需要监听的服务
    pub fn refresh_all(&self, app: &AppHandle) {
        let services = match crate::db::list_services() {
            Ok(s) => s,
            Err(e) => {
                log::error!("读取服务列表失败: {}", e);
                return;
            }
        };
        for s in services {
            if s.auto_restart {
                let _ = self.watch(app.clone(), s);
            } else {
                self.unwatch(&s.id);
            }
        }
    }
}

/// 全局单例
static WATCH: once_cell::sync::Lazy<WatchManager> = once_cell::sync::Lazy::new(WatchManager::new);

pub fn get_watch_manager() -> &'static WatchManager {
    &WATCH
}

fn is_relevant_event(event: &notify::Event) -> bool {
    match event.kind {
        EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_) => {
            for path in &event.paths {
                // 忽略特定目录
                for comp in path.components() {
                    if let std::path::Component::Normal(name) = comp {
                        let s = name.to_string_lossy();
                        if IGNORE_DIRS.contains(&s.as_ref()) {
                            return false;
                        }
                    }
                }
                // 忽略特定扩展名
                if let Some(ext) = path.extension() {
                    if IGNORE_EXTS.contains(&ext.to_string_lossy().as_ref()) {
                        return false;
                    }
                }
            }
            true
        }
        _ => false,
    }
}

fn trigger_restart(app: &AppHandle, service_id: &str) {
    let service = match crate::db::get_service(service_id) {
        Ok(s) => s,
        Err(e) => {
            log::error!("读取服务失败: {}", e);
            return;
        }
    };
    // 仅当服务正在运行时才自动重启
    let mgr = process::get_manager();
    if !mgr.is_running(service_id) {
        return;
    }
    // 竞态防护：检查当前状态，若已在 Recompiling/Starting/Stopping 则跳过，
    // 避免防抖窗口内多次事件触发并发 Maven 编译
    let current_status = mgr.get_runtime(service_id).status;
    if matches!(
        current_status,
        ServiceStatus::Recompiling | ServiceStatus::Starting | ServiceStatus::Stopping
    ) {
        log::info!(
            "跳过自动重启（服务 {} 当前状态: {:?}）",
            service_id,
            current_status
        );
        return;
    }
    let app_clone = app.clone();
    // 异步执行编译启动
    tauri::async_runtime::spawn(async move {
        if let Err(e) = mgr.compile_and_start(app_clone, service).await {
            log::error!("自动重启失败: {}", e);
        }
    });
}
