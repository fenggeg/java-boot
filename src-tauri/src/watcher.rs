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
    /// 该服务监听的模块目录（与 manager 的 working_dir 字符串一致，作 dirty 表 key）
    module_dir: String,
}

pub struct WatchManager {
    watchers: Mutex<HashMap<String, WatchState>>,
    /// 模块源码脏标记：true=有未消费的变更事件。
    /// 供启动策略跳过全树 mtime 扫描（watcher 明确报告干净时）。
    /// key 为 strip 后的 working_dir；无监听（非自动重启服务）时无条目=未知。
    /// Arc 包装：事件回调闭包需要持有引用（'static）
    dirty: std::sync::Arc<Mutex<HashMap<String, bool>>>,
}

impl WatchManager {
    pub fn new() -> Self {
        Self {
            watchers: Mutex::new(HashMap::new()),
            dirty: std::sync::Arc::new(Mutex::new(HashMap::new())),
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
        // 模块目录 key：与 manager.start() 的 working_dir 处理保持一致（剥 verbatim 前缀）
        let module_dir =
            crate::process::build::strip_verbatim_prefix(&PathBuf::from(&service.working_dir))
                .to_string_lossy()
                .to_string();
        let src_main = PathBuf::from(&service.working_dir).join("src").join("main");
        if !src_main.exists() {
            return Err(crate::error::AppError::Other(format!(
                "源码目录不存在: {}",
                src_main.display()
            )));
        }

        // 注册即视为脏：下次启动先做一次真实 mtime 校验，之后由 mark_clean 短路
        self.dirty.lock().insert(module_dir.clone(), true);

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
                    log::error!("watch worker 异常退出 ({}): {:?}，尝试重建监听", sid_worker, e);
                    // 【修复】worker panic 后尝试重建 watcher，避免永久失去自动重启能力
                    // 延迟 5 秒后重建，避免连续 panic 导致 CPU 空转
                    std::thread::sleep(Duration::from_secs(5));
                    if let Ok(svc) = crate::db::get_service(&sid_worker) {
                        if svc.auto_restart {
                            let app_rebuild = app_worker.clone();
                            let _ = get_watch_manager().watch(app_rebuild, svc);
                            log::info!("watch worker 已重建: {}", sid_worker);
                        }
                    }
                }
            })
        };

        let tx_for_cb = tx.clone();
        let dir_for_cb = module_dir.clone();
        let dirty_for_cb = std::sync::Arc::clone(&self.dirty);
        let mut watcher = RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                if let Ok(event) = res {
                    if !is_relevant_event(&event) {
                        return;
                    }
                    // 源码变更：标记模块脏，供启动策略跳过 mtime 全树扫描
                    dirty_for_cb.lock().insert(dir_for_cb.clone(), true);
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
                module_dir,
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
            // 同步清理该模块的脏标记：无监听后状态未知，下次启动回退真实扫描
            self.dirty.lock().remove(&s.module_dir);
            // 先 signal cancel，让 worker 尽快退出
            s._cancel.store(true, Ordering::Relaxed);
            // drop watcher 关闭 channel（worker 的 recv 会返回 Disconnected）
            // join worker 确保线程退出，避免泄漏
            if let Some(handle) = s._worker.take() {
                let _ = handle.join();
            }
        }
        // 清除重启中标志：unwatch 后不应再有重启在执行，避免标志卡住阻塞后续 watch
        RESTART_IN_PROGRESS.lock().remove(service_id);
    }

    /// 停止所有文件监听并回收 worker 线程
    ///
    /// 应用退出时调用：确保 watcher 防抖 worker 不会再触发 `trigger_restart`
    /// （`compile_and_start` 会先 stop 杀进程），避免退出竞态导致服务被误杀。
    pub fn unwatch_all(&self) {
        let states: Vec<(String, WatchState)> = {
            let mut m = self.watchers.lock();
            m.drain().collect()
        };
        self.dirty.lock().clear();
        for (_, mut s) in states {
            s._cancel.store(true, Ordering::Relaxed);
            if let Some(handle) = s._worker.take() {
                let _ = handle.join();
            }
        }
    }

    /// 模块是否可能存在未编译的源码变更：
    /// - 有 watcher 且明确干净（false）→ 返回 false（可跳过 mtime 扫描）
    /// - 无 watcher / 有未消费事件 → true（走真实校验）
    pub fn module_possibly_dirty(&self, module_dir: &str) -> bool {
        !matches!(self.dirty.lock().get(module_dir), Some(false))
    }

    /// 标记模块已构建干净（启动链路确认 classes 就绪后调用）
    pub fn mark_module_clean(&self, module_dir: &str) {
        if self.watchers.lock().values().any(|s| s.module_dir == module_dir) {
            self.dirty.lock().insert(module_dir.to_string(), false);
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

// 竞态防护标志：compile_and_start 正在执行时阻止重复触发
static RESTART_IN_PROGRESS: once_cell::sync::Lazy<Mutex<HashMap<String, bool>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashMap::new()));

fn trigger_restart(app: &AppHandle, service_id: &str) {
    let service = match crate::db::get_service(service_id) {
        Ok(s) => s,
        Err(e) => {
            log::error!("读取服务失败: {}", e);
            return;
        }
    };
    // 仅当服务配置了自动重启时才触发（refresh_all 可能在事件发出后关闭了 auto_restart）
    if !service.auto_restart {
        return;
    }
    // 仅当服务正在运行时才自动重启
    let mgr = process::get_manager();
    if !mgr.is_running(service_id) {
        return;
    }
    // 【TOCTOU 修复】原子地检查状态并设置重启中标志，避免检查与 spawn 之间的竞态
    {
        let mut in_progress = RESTART_IN_PROGRESS.lock();
        if let Some(true) = in_progress.get(service_id) {
            log::info!("跳过自动重启（服务 {} 已有重启在进行中）", service_id);
            return;
        }
        // 再次检查状态（与设置标志原子化）
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
        in_progress.insert(service_id.to_string(), true);
    }
    let app_clone = app.clone();
    let sid_clone = service_id.to_string();
    // 异步执行编译启动
    tauri::async_runtime::spawn(async move {
        if let Err(e) = mgr.compile_and_start(app_clone, service).await {
            log::error!("自动重启失败: {}", e);
        }
        // 清理标志
        RESTART_IN_PROGRESS.lock().remove(&sid_clone);
    });
}
