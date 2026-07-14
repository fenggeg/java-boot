use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;
use tauri::AppHandle;

use crate::db::models::Service;
use crate::process;

const IGNORE_DIRS: &[&str] = &["target", ".idea", "node_modules", ".git", ".vscode"];
const IGNORE_EXTS: &[&str] = &["class"];

struct WatchState {
    _watcher: RecommendedWatcher,
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
        // 已存在则先移除
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

        let mut watcher = RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                if let Ok(event) = res {
                    if !is_relevant_event(&event) {
                        return;
                    }
                    let sid_clone = sid.clone();
                    let app_clone = app.clone();
                    // 在独立线程处理防抖，避免阻塞 watcher
                    std::thread::spawn(move || {
                        handle_debounced(&app_clone, &sid_clone, debounce);
                    });
                }
            },
            Config::default(),
        )
        .map_err(|e| crate::error::AppError::Other(format!("watcher 创建失败: {}", e)))?;

        watcher
            .watch(&src_main, RecursiveMode::Recursive)
            .map_err(|e| crate::error::AppError::Other(format!("监听失败: {}", e)))?;

        self.watchers
            .lock()
            .insert(sid_for_map, WatchState { _watcher: watcher });
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

use std::sync::Mutex as StdMutex;

static DEBOUNCE_TIMERS: once_cell::sync::Lazy<StdMutex<HashMap<String, Arc<Mutex<Option<Instant>>>>>> =
    once_cell::sync::Lazy::new(|| StdMutex::new(HashMap::new()));

/// 防抖处理：debounce 秒内无新事件则触发重启
fn handle_debounced(app: &AppHandle, service_id: &str, debounce: u64) {
    let timer = {
        let mut timers = DEBOUNCE_TIMERS.lock().unwrap();
        timers
            .entry(service_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(None)))
            .clone()
    };

    {
        let mut t = timer.lock();
        *t = Some(Instant::now());
    }

    let sid = service_id.to_string();
    let app_clone = app.clone();
    let timer_clone = timer.clone();
    std::thread::spawn(move || {
        let debounce_dur = Duration::from_secs(debounce);
        loop {
            std::thread::sleep(Duration::from_millis(300));
            let should_fire = {
                let t = timer_clone.lock();
                if let Some(start) = *t {
                    start.elapsed() >= debounce_dur
                } else {
                    false
                }
            };
            if should_fire {
                // 清除 timer
                {
                    let mut t = timer_clone.lock();
                    *t = None;
                }
                trigger_restart(&app_clone, &sid);
                break;
            }
        }
    });
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
