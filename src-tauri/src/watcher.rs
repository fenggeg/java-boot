use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;
use tauri::AppHandle;

use crate::db::models::Service;
use crate::process;

const IGNORE_DIRS: &[&str] = &["target", ".idea", "node_modules", ".git", ".vscode"];
const IGNORE_EXTS: &[&str] = &["class"];

/// 每个服务的防抖 worker：单线程消费事件，避免每次事件都 spawn 新线程
struct WatchState {
    _watcher: RecommendedWatcher,
    /// drop 时关闭 channel，worker 线程随之退出
    _disarm: Arc<Mutex<()>>,
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
        // 已存在则先移除（会 drop 旧 watcher 与 disarm，旧 worker 线程退出）
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

        // 每服务一个 (oneshot 风格的) 信号 channel：事件只做 try_send，worker 做防抖
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let disarm = Arc::new(Mutex::new(()));

        // 启动防抖 worker（单线程）
        {
            let sid_worker = sid.clone();
            let app_worker = app.clone();
            let disarm_worker = disarm.clone();
            std::thread::spawn(move || {
                // 持有 disarm 的引用，确保 unwatch 时通过 drop watcher 关闭 channel
                let _guard = disarm_worker;
                let debounce_dur = Duration::from_secs(debounce);
                // 阻塞等待首个事件
                while rx.recv().is_ok() {
                    // 收到事件后进入防抖循环：直到 debounce 时间内无新事件才触发
                    loop {
                        match rx.recv_timeout(debounce_dur) {
                            Ok(_) => continue, // 又有新事件，重置计时
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    trigger_restart(&app_worker, &sid_worker);
                }
            });
        }

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

        // 保留 tx 以保活 channel（worker 才不会因 recv 返回 Disconnected 提前退出）
        self.watchers.lock().insert(
            sid_for_map,
            WatchState {
                _watcher: watcher,
                _disarm: disarm,
            },
        );
        // 注意：tx 在此函数末尾 drop，但 worker 仍持有 rx；
        // 我们需要让 channel 在 watcher 存活期间保持 open。
        // watcher 回调里持有 tx_for_cb（tx 的克隆），所以 channel 保持 open，
        // 直到 watcher 被 drop（unwatch 时）。worker 在 channel 关闭后退出。
        let _ = tx;
        Ok(())
    }

    pub fn unwatch(&self, service_id: &str) {
        self.watchers.lock().remove(service_id);
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
        EventKind::Modify(_) | EventKind::Create(_) => {
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
    let app_clone = app.clone();
    // 异步执行编译启动
    tauri::async_runtime::spawn(async move {
        if let Err(e) = mgr.compile_and_start(app_clone, service).await {
            log::error!("自动重启失败: {}", e);
        }
    });
}
