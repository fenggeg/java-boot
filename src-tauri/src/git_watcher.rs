//! Git 文件监听与事件推送（防死循环！）。
//!
//! - notify-debouncer-full（500ms 防抖）监听项目目录与真实 git 目录；
//! - `.git` 内只关心 `index` 与 `HEAD`（暂存 / 提交 / 切分支）；
//!   普通源码路径过滤 target / node_modules / dist，避免构建噪音；
//! - `.git` 可能是文件（worktree / submodule）→ 用 `git rev-parse --absolute-git-dir`
//!   解析真实 git 目录并额外监听；
//! - 自身 status 已用 `--no-optional-locks`（不写 index），不会触发自身监听；
//!   仍以「结果 hash 去重」兜底——内容无变化则不 emit；
//! - 有实质变化时 `app.emit("git://changed", ())` 推送前端。

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use notify_debouncer_full::{
    new_debouncer, DebounceEventResult, DebouncedEvent, Debouncer, RecommendedCache,
    notify::{RecursiveMode, RecommendedWatcher},
};
use parking_lot::Mutex;
use tauri::{AppHandle, Emitter};

use crate::db;
use crate::git_cli;
use crate::util::canonicalize_clean;

const GIT_DEBOUNCE_MS: u64 = 500;
const IGNORE_DIRS: &[&str] = &["target", "node_modules", "dist"];

pub struct GitWatchManager {
    /// setup 中注入的 app handle（供事件推送；command 层无需再传）
    app: Mutex<Option<AppHandle>>,
    watchers: Mutex<HashMap<String, GitWatchState>>,
    /// 每个项目的最近一次 status hash（内容去重，防死循环兜底）
    last_hash: Arc<Mutex<HashMap<String, u64>>>,
}

struct GitWatchState {
    _debouncer: Debouncer<RecommendedWatcher, RecommendedCache>,
}

impl GitWatchManager {
    pub fn new() -> Self {
        Self {
            app: Mutex::new(None),
            watchers: Mutex::new(HashMap::new()),
            last_hash: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// setup 中调用：注入 app handle
    pub fn init(&self, app: &AppHandle) {
        *self.app.lock() = Some(app.clone());
    }

    /// setup / add_project 中调用：为所有已添加项目注册 git 监听（幂等）
    pub fn refresh_all(&self) {
        let projects = db::list_projects().unwrap_or_default();
        for p in projects {
            self.watch(&p.root_path);
        }
    }

    /// 为单个项目注册 git 监听（非 git 仓库静默跳过；幂等）
    pub fn watch(&self, project_root: &str) {
        let root = match canonicalize_clean(Path::new(project_root)) {
            Some(r) => r,
            None => return,
        };
        let key = root.to_string_lossy().to_string();
        if self.watchers.lock().contains_key(&key) {
            return;
        }
        // 解析真实 git 目录；非仓库 → 无 git UI，无需监听
        let gitdir = match git_cli::git_dir(&root) {
            Some(g) => canonicalize_clean(&g).unwrap_or(g),
            None => return,
        };
        // 事件推送需要 app handle；未 init 时跳过（测试等场景）
        let app = match self.app.lock().clone() {
            Some(a) => a,
            None => return,
        };

        let app_cb = app.clone();
        let root_cb = root.clone();
        let gitdir_cb = gitdir.clone();
        let key_cb = key.clone();
        let last_hash_cb = self.last_hash.clone();
        let debouncer = match new_debouncer(
            Duration::from_millis(GIT_DEBOUNCE_MS),
            None,
            move |result: DebounceEventResult| {
                let Ok(events) = result else { return };
                if !has_relevant(&root_cb, &gitdir_cb, &events) {
                    return;
                }
                recompute_and_emit(&app_cb, &key_cb, &root_cb, &last_hash_cb);
            },
        ) {
            Ok(d) => d,
            Err(e) => {
                log::warn!("创建 git 监听失败 {}: {}", root.display(), e);
                return;
            }
        };
        let mut debouncer = debouncer;
        if let Err(e) = debouncer.watch(&root, RecursiveMode::Recursive) {
            log::warn!("监听项目目录失败 {}: {}", root.display(), e);
            return;
        }
        // worktree / submodule：真实 git 目录在项目内 `.git` 之外，需额外监听
        if gitdir != root.join(".git") {
            if let Err(e) = debouncer.watch(&gitdir, RecursiveMode::Recursive) {
                log::warn!("监听 git 目录失败 {}: {}", gitdir.display(), e);
            }
        }
        self.watchers
            .lock()
            .insert(key, GitWatchState { _debouncer: debouncer });
    }

    /// 停止指定项目监听（delete_project 时调用）
    pub fn unwatch(&self, project_root: &str) {
        let root = match canonicalize_clean(Path::new(project_root)) {
            Some(r) => r,
            None => return,
        };
        let key = root.to_string_lossy().to_string();
        self.watchers.lock().remove(&key);
        self.last_hash.lock().remove(&key);
    }

    /// 应用退出时调用：停止所有 git 监听（drop debouncer 即停止）
    pub fn unwatch_all(&self) {
        self.watchers.lock().clear();
        self.last_hash.lock().clear();
    }
}

/// 事件中是否有值得刷新的路径
fn has_relevant(project_root: &Path, gitdir: &Path, events: &[DebouncedEvent]) -> bool {
    events
        .iter()
        .any(|e| e.paths.iter().any(|p| path_is_relevant(project_root, gitdir, p)))
}

/// 路径过滤：
/// - 在 git 目录内 → 只关心 index / HEAD；
/// - 项目内普通路径 → 过滤 target / node_modules / dist / .git
fn path_is_relevant(project_root: &Path, gitdir: &Path, p: &Path) -> bool {
    if p.starts_with(gitdir) {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        return name == "index" || name == "HEAD";
    }
    if let Ok(rel) = p.strip_prefix(project_root) {
        for seg in rel.components() {
            let s = seg.as_os_str().to_string_lossy();
            if IGNORE_DIRS.iter().any(|d| *d == s) || s == ".git" {
                return false;
            }
        }
    }
    true
}

/// 重算该仓库 status，与上次结果 hash 对比：有实质变化才 emit（防死循环兜底）
fn recompute_and_emit(
    app: &AppHandle,
    key: &str,
    root: &Path,
    last_hash: &Arc<Mutex<HashMap<String, u64>>>,
) {
    let entries = match git_cli::status_all(root) {
        Ok(v) => v,
        Err(e) => {
            log::debug!("重算 git status 失败 {}: {}", root.display(), e);
            return;
        }
    };
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    entries.hash(&mut hasher);
    let h = hasher.finish();
    let prev = {
        let mut map = last_hash.lock();
        map.insert(key.to_string(), h)
    };
    if prev == Some(h) {
        return; // 内容无变化，不 emit
    }
    let _ = app.emit("git://changed", ());
}

/// 全局单例
pub fn get_git_watch_manager() -> &'static GitWatchManager {
    static M: once_cell::sync::OnceCell<GitWatchManager> = once_cell::sync::OnceCell::new();
    M.get_or_init(GitWatchManager::new)
}
