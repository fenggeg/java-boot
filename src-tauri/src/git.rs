use parking_lot::Mutex as SMutex;
use std::collections::HashMap;
use std::io::BufRead;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, LazyLock, OnceLock};

use tauri::AppHandle;

use crate::db;
use crate::error::{AppError, AppResult};
use crate::process;
use crate::util::NoWindow;

/// 拉取结果
#[derive(Debug, Clone, serde::Serialize)]
pub struct PullResult {
    pub project_id: String,
    pub success: bool,
    pub up_to_date: bool,
    pub message: String,
}

pub fn git_available() -> bool {
    resolve_git().is_some()
}

/// git 可执行文件路径缓存：PATH 扫描 + `git --version` 探测要 spawn 多个子进程，
/// 每次 status/diff/log 重复执行会让文件树 git 标记明显延迟，进程生命周期内只解析一次
static GIT_EXE: OnceLock<Option<String>> = OnceLock::new();

/// 定位 git 可执行文件（带缓存）：PATH 优先，fallback 到 scoop shims
pub fn resolve_git() -> Option<String> {
    GIT_EXE.get_or_init(resolve_git_uncached).clone()
}

fn resolve_git_uncached() -> Option<String> {
    // 1. 在 PATH 中逐目录查找 git.exe（比 Command::new("git") 更可靠，
    //    避免 Tauri 进程 PATH 搜索行为差异）
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let git_exe = dir.join("git.exe");
            if git_exe.exists() {
                if Command::new(&git_exe)
                    .arg("--version")
                    .creation_flags_no_window()
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
                {
                    return Some(git_exe.to_string_lossy().to_string());
                }
            }
        }
    }
    log::warn!("resolve_git: PATH 中未找到可用的 git.exe");
    // 2. scoop shims（打包后应用可能不继承用户 PATH）
    if let Ok(home) = std::env::var("USERPROFILE") {
        let scoop_git = format!("{}\\scoop\\shims\\git.exe", home);
        if std::path::Path::new(&scoop_git).exists() {
            if Command::new(&scoop_git)
                .arg("--version")
                .creation_flags_no_window()
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
            {
                return Some(scoop_git);
            }
            log::warn!("resolve_git: scoop git 存在但执行失败: {}", scoop_git);
        }
        // 3. UGit 自带的 git（用户 PATH 中有 UGit 路径）。
        //    版本目录 app-* 随 UGit 升级变化，不再硬编码版本号，改为扫描目录
        let ugit_base = format!("{}\\AppData\\Local\\UGit", home);
        if let Ok(entries) = std::fs::read_dir(&ugit_base) {
            for entry in entries.flatten() {
                let dir = entry.path();
                if !entry.file_name().to_string_lossy().starts_with("app-") {
                    continue;
                }
                if !dir.is_dir() {
                    continue;
                }
                let ugit_git = dir
                    .join("resources")
                    .join("app")
                    .join("git")
                    .join("cmd")
                    .join("git.exe");
                if !ugit_git.exists() {
                    continue;
                }
                if Command::new(&ugit_git)
                    .arg("--version")
                    .creation_flags_no_window()
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
                {
                    return Some(ugit_git.to_string_lossy().to_string());
                }
            }
        }
    }
    None
}

/// 检测目录是否为 Git 仓库
pub fn is_git_repo(path: &Path) -> bool {
    path.join(".git").exists()
}

/// 执行 git pull
pub async fn pull(app: AppHandle, project_id: &str) -> AppResult<PullResult> {
    let project = db::get_project(project_id)?;
    let root = Path::new(&project.root_path);

    if !is_git_repo(root) {
        return Err(AppError::Git(format!(
            "{} 不是 Git 仓库",
            root.display()
        )));
    }

    // 互斥检查：项目下任一服务正在编译/启动则禁用
    let services = db::list_services_by_project(project_id)?;
    for s in &services {
        let rt = process::get_manager().get_runtime(&s.id);
        if matches!(
            rt.status,
            db::models::ServiceStatus::Starting
                | db::models::ServiceStatus::Recompiling
                | db::models::ServiceStatus::Pulling
        ) {
            return Err(AppError::Git(format!(
                "项目下服务 {} 正在启动/编译中，请稍后重试",
                s.name
            )));
        }
    }

    // 标记项目下所有服务为"拉取中"
    for s in &services {
        process::get_manager().set_status(
            &app,
            &s.id,
            db::models::ServiceStatus::Pulling,
        );
    }

    let root_clone = root.to_path_buf();
    let project_id_owned = project_id.to_string();
    let app_clone = app.clone();
    let services_clone = services.clone();

    // 定位 git（PATH 优先，fallback scoop shims）
    let git_exe = match resolve_git() {
        Some(g) => g,
        None => {
            return Err(AppError::Git(
                "未找到 git 命令，请安装 Git 并加入 PATH".to_string(),
            ));
        }
    };

    // git pull 可能因需要认证而永久阻塞 spawn_blocking 线程，
    // 用 tokio::time::timeout 包裹，60 秒超时。
    // 共享 stdout/stderr 缓冲与子进程 PID：超时后强杀 git 进程树，
    // 避免它继续在后台拉取、静默改写工作区。
    let stdout_shared: Arc<SMutex<String>> = Arc::new(SMutex::new(String::new()));
    let stderr_shared: Arc<SMutex<String>> = Arc::new(SMutex::new(String::new()));
    let pid_slot: Arc<SMutex<Option<u32>>> = Arc::new(SMutex::new(None));

    let stdout_shared_b = stdout_shared.clone();
    let stderr_shared_b = stderr_shared.clone();
    let pid_slot_b = pid_slot.clone();
    let services_b = services.clone();
    let app_b = app.clone();

    let pull_timeout = std::time::Duration::from_secs(60);
    let join_result = tokio::time::timeout(
        pull_timeout,
        tokio::task::spawn_blocking(move || {
            // 禁止交互式凭据提示，避免私有仓库卡住
            let mut cmd = Command::new(&git_exe);
            cmd.arg("pull")
                .arg("--no-progress")
                .current_dir(&root_clone)
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("GIT_ASKPASS", "")
                .env("SSH_ASKPASS", "");
            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());
            cmd.stdin(std::process::Stdio::null());
            cmd.creation_flags_no_window();
            let mut child = cmd.spawn()?;
            // 立即记录 PID，主流程在超时时能据此杀进程
            *pid_slot_b.lock() = Some(child.id());
            let out = child.stdout.take();
            let err = child.stderr.take();
            // 两个读线程分别消费 stdout/stderr 管道，防止管道缓冲满导致 git 阻塞；
            // 输出直接推送日志，同时收集到共享缓冲供主流程判断 up-to-date。
            let t_out = {
                let stdout_shared = stdout_shared_b.clone();
                let services = services_b.clone();
                let app = app_b.clone();
                std::thread::spawn(move || {
                    if let Some(o) = out {
                        let reader = std::io::BufReader::new(o);
                        for line in reader.lines().flatten() {
                            stdout_shared.lock().push_str(&line);
                            stdout_shared.lock().push('\n');
                            for s in &services {
                                process::ProcessManager::emit_log_static(&app, &s.id, "[git]", &line);
                            }
                        }
                    }
                })
            };
            let t_err = {
                let stderr_shared = stderr_shared_b.clone();
                let services = services_b.clone();
                let app = app_b.clone();
                std::thread::spawn(move || {
                    if let Some(e) = err {
                        let reader = std::io::BufReader::new(e);
                        for line in reader.lines().flatten() {
                            stderr_shared.lock().push_str(&line);
                            stderr_shared.lock().push('\n');
                            for s in &services {
                                process::ProcessManager::emit_log_static(&app, &s.id, "[git]", &line);
                            }
                        }
                    }
                })
            };
            let status = child.wait();
            let _ = t_out.join();
            let _ = t_err.join();
            status
        }),
    )
    .await;

    let result = match join_result {
        Ok(Ok(output)) => output.map_err(|e| AppError::Git(format!("git pull 执行失败: {}", e)))?,
        Ok(Err(e)) => return Err(AppError::Git(format!("git pull 任务失败: {}", e))),
        Err(_) => {
            // 超时：强杀 git 进程树（含子进程），避免继续在后台拉取改写工作区
            if let Some(pid) = pid_slot.lock().take() {
                crate::process::manager::kill_process_tree_by_pid(pid);
                log::warn!("git pull 超时，已强杀 git 进程 PID {}", pid);
            }
            // 恢复服务状态并返回错误
            for s in &services_clone {
                let rt = process::get_manager().get_runtime(&s.id);
                let new_status = if rt.pid.is_some() {
                    db::models::ServiceStatus::Running
                } else {
                    db::models::ServiceStatus::Stopped
                };
                process::get_manager().set_status(&app_clone, &s.id, new_status);
            }
            return Err(AppError::Git(
                "git pull 超时（60 秒），可能需要认证或网络异常".to_string(),
            ));
        }
    };

    let stdout = stdout_shared.lock().clone();
    let stderr = stderr_shared.lock().clone();
    let success = result.success();

    let up_to_date = stdout.contains("Already up to date")
        || stdout.contains("Already up-to-date")
        || stdout.contains("已经是最新")
        || stdout.contains("已是最新")
        || stdout.contains("up to date")
        || stdout.contains("up-to-date");

    // 日志已在读线程中实时推送，这里只恢复各服务状态
    for s in &services {
        let rt = process::get_manager().get_runtime(&s.id);
        let new_status = if rt.pid.is_some() {
            db::models::ServiceStatus::Running
        } else {
            db::models::ServiceStatus::Stopped
        };
        process::get_manager().set_status(&app, &s.id, new_status);
    }

    let message = if success {
        if up_to_date {
            "已是最新".to_string()
        } else {
            stdout.trim().to_string()
        }
    } else {
        format!("{}\n{}", stdout.trim(), stderr.trim())
    };

    let res = PullResult {
        project_id: project_id_owned,
        success,
        up_to_date,
        message,
    };
    Ok(res)
}

/// 拉取并重启项目下运行中的服务
pub async fn pull_and_restart(app: AppHandle, project_id: &str) -> AppResult<PullResult> {
    let result = pull(app.clone(), project_id).await?;
    if result.success {
        let services = db::list_services_by_project(project_id)?;
        let mgr = process::get_manager();
        for s in services {
            // 仅重启正在运行的服务
            if mgr.is_running(&s.id) {
                if let Err(e) = mgr.compile_and_start(app.clone(), s).await {
                    log::error!("拉取后重启失败: {}", e);
                }
            }
        }
    }
    Ok(result)
}

/// 推送结果（复用 PullResult 结构：success/message；up_to_date 恒为 false）
#[derive(Debug, Clone, serde::Serialize)]
pub struct PushResult {
    pub project_id: String,
    pub success: bool,
    pub message: String,
}

/// 执行 git push
///
/// push 不改动工作区，无需切换服务状态；但可能因认证挂起，
/// 与 pull 相同采用 60 秒超时 + 超时强杀进程树。
pub async fn push(app: AppHandle, project_id: &str) -> AppResult<PushResult> {
    let project = db::get_project(project_id)?;
    let root = Path::new(&project.root_path);

    if !is_git_repo(root) {
        return Err(AppError::Git(format!(
            "{} 不是 Git 仓库",
            root.display()
        )));
    }

    let git_exe = resolve_git().ok_or_else(|| {
        AppError::Git("未找到 git 命令，请安装 Git 并加入 PATH".to_string())
    })?;

    let root_clone = root.to_path_buf();
    let project_id_owned = project_id.to_string();

    // 共享输出缓冲与 PID 槽，超时后强杀
    let stdout_shared: Arc<SMutex<String>> = Arc::new(SMutex::new(String::new()));
    let stderr_shared: Arc<SMutex<String>> = Arc::new(SMutex::new(String::new()));
    let pid_slot: Arc<SMutex<Option<u32>>> = Arc::new(SMutex::new(None));
    let stdout_b = stdout_shared.clone();
    let stderr_b = stderr_shared.clone();
    let pid_slot_b = pid_slot.clone();

    let join_result = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        tokio::task::spawn_blocking(move || {
            let mut cmd = Command::new(&git_exe);
            cmd.arg("push")
                .arg("--no-progress")
                .arg("--porcelain")
                .current_dir(&root_clone)
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("GIT_ASKPASS", "")
                .env("SSH_ASKPASS", "");
            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());
            cmd.stdin(std::process::Stdio::null());
            cmd.creation_flags_no_window();
            let mut child = cmd.spawn()?;
            *pid_slot_b.lock() = Some(child.id());
            let out = child.stdout.take();
            let err = child.stderr.take();
            let t_out = std::thread::spawn(move || {
                if let Some(o) = out {
                    let reader = std::io::BufReader::new(o);
                    for line in reader.lines().flatten() {
                        stdout_b.lock().push_str(&line);
                        stdout_b.lock().push('\n');
                    }
                }
            });
            let t_err = std::thread::spawn(move || {
                if let Some(e) = err {
                    let reader = std::io::BufReader::new(e);
                    for line in reader.lines().flatten() {
                        stderr_b.lock().push_str(&line);
                        stderr_b.lock().push('\n');
                    }
                }
            });
            let status = child.wait();
            let _ = t_out.join();
            let _ = t_err.join();
            status
        }),
    )
    .await;

    let result = match join_result {
        Ok(Ok(output)) => output.map_err(|e| AppError::Git(format!("git push 执行失败: {}", e)))?,
        Ok(Err(e)) => return Err(AppError::Git(format!("git push 任务失败: {}", e))),
        Err(_) => {
            if let Some(pid) = pid_slot.lock().take() {
                crate::process::manager::kill_process_tree_by_pid(pid);
                log::warn!("git push 超时，已强杀 git 进程 PID {}", pid);
            }
            return Err(AppError::Git(
                "git push 超时（60 秒），可能需要认证或网络异常".to_string(),
            ));
        }
    };

    let _ = app; // 预留：后续推送进度事件
    let stdout = stdout_shared.lock().clone();
    let stderr = stderr_shared.lock().clone();
    let success = result.success();

    // 失败信息归类：非快进 / 无上游 / 认证失败等给出可操作提示
    let message = if success {
        "推送成功".to_string()
    } else {
        let combined = format!("{}\n{}", stdout, stderr);
        if combined.contains("(non-fast-forward)") || combined.contains("fetch first") {
            "推送被拒绝（远程有新提交），请先拉取合并后再推送".to_string()
        } else if combined.contains("No configured push destination")
            || combined.contains("does not have an upstream")
        {
            "当前分支未设置上游分支，请在 IDEA/命令行配置后重试".to_string()
        } else if combined.contains("Authentication") || combined.contains("403") {
            "推送认证失败，请检查凭据或令牌权限".to_string()
        } else {
            format!("{}\n{}", stdout.trim(), stderr.trim())
        }
    };

    Ok(PushResult {
        project_id: project_id_owned,
        success,
        message,
    })
}

// ================================================================
// 工作区状态 / 提交 / 历史（按项目隔离）
//
// 所有路径参数均为相对 repo root 的 POSIX 风格路径（`git status -z` 输出），
// 内部统一用 `-C <root>` 执行 git 命令，避免进程工作目录漂移。
// ================================================================

/// 运行 git 命令，非零退出码时返回 stderr（或 stdout）作为错误信息
fn run_git(root: &Path, args: &[&str]) -> AppResult<std::process::Output> {
    let git = resolve_git().ok_or_else(|| AppError::Git("未找到可用的 git".to_string()))?;
    let mut cmd = Command::new(&git);
    // core.quotepath=false：让中文路径以原始 UTF-8 输出，避免 \xxx 转义
    cmd.arg("-c")
        .arg("core.quotepath=false")
        .arg("-C")
        .arg(root);
    cmd.args(args);
    cmd.creation_flags_no_window();
    cmd.stdin(std::process::Stdio::null());
    let out = cmd
        .output()
        .map_err(|e| AppError::Git(format!("git 执行失败: {}", e)))?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let msg = if msg.is_empty() {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        } else {
            msg
        };
        return Err(AppError::Git(if msg.is_empty() {
            format!("git {:?} 失败", args.first().copied().unwrap_or(""))
        } else {
            msg
        }));
    }
    Ok(out)
}

/// 项目 → 真实 repo root 缓存：省去每次 `git rev-parse --show-toplevel` 的进程 spawn，
/// 命中后仅做一次 `.git` 存在性检查（文件/目录均适用，覆盖 worktree），仓库被移除时自动失效
static REPO_ROOTS: LazyLock<SMutex<HashMap<String, PathBuf>>> =
    LazyLock::new(|| SMutex::new(HashMap::new()));

/// 项目根 → 真实 repo root（`git rev-parse --show-toplevel`，跟随 worktree/submodule）
fn repo_root(project_id: &str) -> AppResult<PathBuf> {
    {
        let cache = REPO_ROOTS.lock();
        if let Some(p) = cache.get(project_id) {
            if p.join(".git").exists() {
                return Ok(p.clone());
            }
        }
    }
    let project = db::get_project(project_id)?;
    let root = Path::new(&project.root_path);
    if !is_git_repo(root) {
        return Err(AppError::Git(format!("{} 不是 Git 仓库", root.display())));
    }
    let out = run_git(root, &["rev-parse", "--show-toplevel"])?;
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if p.is_empty() {
        return Err(AppError::Git("无法解析 Git 仓库根目录".to_string()));
    }
    let resolved = PathBuf::from(p);
    REPO_ROOTS
        .lock()
        .insert(project_id.to_string(), resolved.clone());
    Ok(resolved)
}

/// 将相对路径安全解析为 repo root 下的绝对路径，阻止绝对路径 / `..` 穿越
fn safe_join(root: &Path, rel: &str) -> AppResult<PathBuf> {
    let p = Path::new(rel);
    if p.is_absolute() {
        return Err(AppError::Git("不允许绝对路径".to_string()));
    }
    if p.components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(AppError::Git("不允许路径穿越 (..)".to_string()));
    }
    let full = root.join(p);
    if let (Some(r), Some(f)) = (
        crate::util::canonicalize_clean(root),
        crate::util::canonicalize_clean(&full),
    ) {
        if !f.starts_with(&r) {
            return Err(AppError::Git("路径越界".to_string()));
        }
    }
    Ok(full)
}

/// 单个文件改动（`git status --porcelain=v1` 的 XY 解析结果）
#[derive(serde::Serialize)]
pub struct GitChange {
    pub path: String,
    pub old_path: Option<String>,
    /// 暂存区状态码（X）：` `=未改, `M`/`A`/`D`/`R`/`C`/`U`/`?`
    pub x: String,
    /// 工作区状态码（Y）
    pub y: String,
    pub staged: bool,
    pub tracked: bool,
}

/// 项目当前工作区状态
#[derive(serde::Serialize)]
pub struct GitStatus {
    pub branch: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    /// 是否处于合并中（存在 MERGE_HEAD，即 pull/merge 产生待解决的冲突）
    pub merging: bool,
    pub changes: Vec<GitChange>,
}

/// 提交记录
#[derive(serde::Serialize)]
pub struct GitCommitInfo {
    pub hash: String,
    pub short_hash: String,
    pub author: String,
    pub date: String,
    pub message: String,
}

/// 工作区状态：分支 / 领先落后 / 全部改动文件
pub fn status(project_id: &str) -> AppResult<GitStatus> {
    let root = repo_root(project_id)?;
    let out = run_git(&root, &["status", "--porcelain=v1", "-z", "--branch"])?;
    let raw = out.stdout;
    let parts: Vec<&[u8]> = raw.split(|b| *b == 0).filter(|s| !s.is_empty()).collect();

    let mut branch = None;
    let mut ahead = 0u32;
    let mut behind = 0u32;
    let mut changes = vec![];
    let mut idx = 0usize;

    // 第一个记录可能是 branch 头（`## main...origin/main [ahead 1, behind 2]`）
    if let Some(first) = parts.first() {
        if first.starts_with(b"## ") {
            let line = String::from_utf8_lossy(first).trim().to_string();
            let rest = &line[3..];
            let bracket = rest.find('[');
            let (name_part, meta) = match bracket {
                Some(i) => (rest[..i].trim(), rest[i..].to_string()),
                None => (rest.trim(), String::new()),
            };
            if !name_part.starts_with("HEAD") {
                branch = Some(
                    name_part
                        .split("...")
                        .next()
                        .unwrap_or(name_part)
                        .trim()
                        .to_string(),
                );
            }
            for seg in meta.trim_start_matches('[').trim_end_matches(']').split(',') {
                let seg = seg.trim();
                if let Some(n) = seg.strip_prefix("ahead ") {
                    ahead = n.trim().parse().unwrap_or(0);
                } else if let Some(n) = seg.strip_prefix("behind ") {
                    behind = n.trim().parse().unwrap_or(0);
                }
            }
            idx = 1;
        }
    }

    while idx < parts.len() {
        let rec = parts[idx];
        // 记录格式：`XY path`；跳过 malformed（如空段）
        if rec.len() < 3 {
            idx += 1;
            continue;
        }
        let x = rec[0] as char;
        let y = rec[1] as char;
        let mut path = String::from_utf8_lossy(&rec[3..]).to_string();
        let mut old_path = None;
        // rename/copy：`R  old` NUL `new`，下一段为最终路径
        if x == 'R' || x == 'C' {
            old_path = Some(path.clone());
            if idx + 1 < parts.len() {
                path = String::from_utf8_lossy(parts[idx + 1]).to_string();
                idx += 1;
            }
        }
        changes.push(GitChange {
            path,
            old_path,
            x: x.to_string(),
            y: y.to_string(),
            staged: x != ' ' && x != '?',
            tracked: !(x == '?' && y == '?'),
        });
        idx += 1;
    }

    Ok(GitStatus {
        branch,
        ahead,
        behind,
        merging: merge_in_progress(&root),
        changes,
    })
}

/// 是否处于合并中：`git rev-parse --git-path MERGE_HEAD` 存在
/// （跟随 worktree/submodule 的真实 git 目录）
fn merge_in_progress(root: &Path) -> bool {
    let out = match run_git(root, &["rev-parse", "--git-path", "MERGE_HEAD"]) {
        Ok(o) => o,
        Err(_) => return false,
    };
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if p.is_empty() {
        return false;
    }
    let p = PathBuf::from(&p);
    let full = if p.is_absolute() { p } else { root.join(p) };
    full.exists()
}

/// 单文件 diff：
/// - `staged=true`：暂存区 vs HEAD（本次将提交的内容）
/// - `staged=false`：工作区 vs HEAD（全部未提交改动，与 diff_hunks /
///   文件树行级标记完全同基准，保证 Git 面板与编辑器的新增行数一致）
/// 两种模式都忽略行尾 CR（抵消 core.autocrlf 的换行符噪声）；
/// 空仓库尚无 HEAD 提交时，工作区模式回退为 vs 暂存区。
pub fn diff(project_id: &str, path: &str, staged: bool) -> AppResult<String> {
    let root = repo_root(project_id)?;
    safe_join(&root, path)?; // 仅校验路径合法性
    if staged {
        let out = run_git(
            &root,
            &[
                "diff",
                "--cached",
                "--no-color",
                "--unified=3",
                "--ignore-cr-at-eol",
                "--",
                path,
            ],
        )?;
        return Ok(crate::util::decode_output(&out.stdout));
    }
    let out = run_git(
        &root,
        &[
            "diff",
            "HEAD",
            "--no-color",
            "--unified=3",
            "--ignore-cr-at-eol",
            "--",
            path,
        ],
    );
    match out {
        Ok(out) => Ok(crate::util::decode_output(&out.stdout)),
        Err(_) => {
            // 多半是无提交历史的空仓库（bad revision HEAD），回退 vs 暂存区
            let out = run_git(
                &root,
                &[
                    "diff",
                    "--no-color",
                    "--unified=3",
                    "--ignore-cr-at-eol",
                    "--",
                    path,
                ],
            )?;
            Ok(crate::util::decode_output(&out.stdout))
        }
    }
}

/// 暂存指定文件
pub fn stage(project_id: &str, paths: &[String]) -> AppResult<()> {
    let root = repo_root(project_id)?;
    for p in paths {
        safe_join(&root, p)?;
    }
    let mut args: Vec<&str> = vec!["add", "--"];
    args.extend(paths.iter().map(|s| s.as_str()));
    run_git(&root, &args)?;
    Ok(())
}

/// 取消暂存指定文件（`git restore --staged`，工作区内容不动）
pub fn unstage(project_id: &str, paths: &[String]) -> AppResult<()> {
    let root = repo_root(project_id)?;
    for p in paths {
        safe_join(&root, p)?;
    }
    let mut args: Vec<&str> = vec!["restore", "--staged", "--"];
    args.extend(paths.iter().map(|s| s.as_str()));
    run_git(&root, &args)?;
    Ok(())
}

/// 提交暂存区
pub fn commit(project_id: &str, message: &str) -> AppResult<()> {
    let msg = message.trim();
    if msg.is_empty() {
        return Err(AppError::Git("提交信息不能为空".to_string()));
    }
    let root = repo_root(project_id)?;
    run_git(&root, &["commit", "-m", msg])?;
    Ok(())
}

/// 最近提交记录
pub fn log(project_id: &str, limit: u32) -> AppResult<Vec<GitCommitInfo>> {
    let root = repo_root(project_id)?;
    let n = limit.min(200);
    let n_str = n.to_string();
    let out = run_git(
        &root,
        &[
            "log",
            "--no-color",
            "-n",
            n_str.as_str(),
            "--pretty=format:%H%x1f%h%x1f%an%x1f%aI%x1f%s",
        ],
    )?;
    let text = crate::util::decode_output(&out.stdout);
    let mut commits = vec![];
    for line in text.lines() {
        let f: Vec<&str> = line.split('\x1f').collect();
        if f.len() >= 5 {
            commits.push(GitCommitInfo {
                hash: f[0].to_string(),
                short_hash: f[1].to_string(),
                author: f[2].to_string(),
                date: f[3].to_string(),
                message: f[4..].join("\x1f"),
            });
        }
    }
    Ok(commits)
}

/// 查看指定提交的完整 diff
pub fn show(project_id: &str, hash: &str) -> AppResult<String> {
    let root = repo_root(project_id)?;
    if hash.is_empty() || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(AppError::Git("无效的提交哈希".to_string()));
    }
    let out = run_git(&root, &["show", "--no-color", "--unified=3", hash])?;
    Ok(crate::util::decode_output(&out.stdout))
}

/// 读取工作区文件内容（仅 UTF-8 文本，限制 2MB 防误读大文件/二进制）
pub fn read_file(project_id: &str, path: &str) -> AppResult<String> {
    let root = repo_root(project_id)?;
    let full = safe_join(&root, path)?;
    if !full.exists() {
        return Err(AppError::Git(format!("文件不存在: {}", path)));
    }
    let bytes = std::fs::read(&full).map_err(|e| AppError::Git(format!("读取失败: {}", e)))?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(AppError::Git("文件超过 2MB，暂不支持编辑".to_string()));
    }
    String::from_utf8(bytes).map_err(|_| {
        AppError::Git("文件不是 UTF-8 文本，暂不支持编辑".to_string())
    })
}

/// 写回工作区文件内容
pub fn write_file(project_id: &str, path: &str, content: &str) -> AppResult<()> {
    let root = repo_root(project_id)?;
    let full = safe_join(&root, path)?;
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AppError::Git(format!("创建目录失败: {}", e)))?;
    }
    std::fs::write(&full, content).map_err(|e| AppError::Git(format!("写入失败: {}", e)))
}

/// 读取 HEAD 中某文件的内容（用于前端做工作区 vs HEAD 的行级 diff 标记）。
/// 文件不在 HEAD（未跟踪 / 新增）或读取失败时返回 None。
pub fn file_at_head(project_id: &str, path: &str) -> AppResult<Option<String>> {
    let root = repo_root(project_id)?;
    safe_join(&root, path)?;
    match run_git(&root, &["show", &format!("HEAD:{}", path)]) {
        Ok(out) => Ok(Some(crate::util::decode_output(&out.stdout))),
        Err(_) => Ok(None),
    }
}

/// 单个 diff 块（unified=0）：新文件中起始于 new_start（1 基）共 new_lines 行，
/// 对应旧文件删除了 del_lines 行；min(del,new) 行视为「修改」，其余新增为「新增」。
#[derive(Debug, Clone, serde::Serialize)]
pub struct DiffHunk {
    pub new_start: u32,
    pub new_lines: u32,
    pub del_lines: u32,
}

/// 解析 unified=0 diff 文本的 hunk 头与 +/- 行计数。
/// 注意：头部 `+start,count` 的 count 与 body 的 +/- 行数相同，
/// 这里只从头取起始位置，行数一律由 body 实际计数，避免重复累加。
fn parse_diff_hunks(text: &str) -> Vec<DiffHunk> {
    fn parse_new_start(rest: &str) -> Option<u32> {
        // rest 形如 " -66,0 +67,4 @@ ctx..."
        for tok in rest.split_whitespace() {
            if let Some(stripped) = tok.strip_prefix('+') {
                return stripped.split(',').next()?.parse().ok();
            }
        }
        None
    }

    let mut hunks: Vec<DiffHunk> = vec![];
    let mut cur: Option<DiffHunk> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("@@") {
            if let Some(h) = cur.take() {
                hunks.push(h);
            }
            cur = parse_new_start(rest).map(|new_start| DiffHunk {
                new_start,
                new_lines: 0,
                del_lines: 0,
            });
            continue;
        }
        if let Some(h) = cur.as_mut() {
            if line.starts_with('+') {
                h.new_lines += 1;
            } else if line.starts_with('-') {
                h.del_lines += 1;
            }
            // "\ No newline at end of file" 以 \ 开头，忽略
        }
    }
    if let Some(h) = cur.take() {
        hunks.push(h);
    }
    hunks
}

/// 工作区 vs HEAD 的 diff hunk 列表（与 Git 面板同一 diff 引擎，保证一致）。
/// `--ignore-cr-at-eol` 抵消 core.autocrlf 造成的 CRLF/LF 差异；
/// 未跟踪文件 git diff 输出为空，由前端按「整文件新增」处理。
pub fn diff_hunks(project_id: &str, path: &str) -> AppResult<Vec<DiffHunk>> {
    let root = repo_root(project_id)?;
    safe_join(&root, path)?;
    let out = run_git(
        &root,
        &[
            "diff",
            "HEAD",
            "--no-color",
            "--unified=0",
            "--ignore-cr-at-eol",
            "--",
            path,
        ],
    )?;
    Ok(parse_diff_hunks(&crate::util::decode_output(&out.stdout)))
}

// ================================================================
// 冲突合并（pull/merge 产生冲突后的解决流程）
//
// git 在合并冲突时把文件的三个版本存入 index：
//   :1:path = 共同祖先(base)  :2:path = 本地(ours)  :3:path = 远程(theirs)
// 前端据此渲染三栏对比，用户逐个文件解决后 `git add` 标记，
// 全部解决后 `git commit` 完成合并。
// ================================================================

/// 判断某改动记录是否为冲突状态（porcelain v1 的冲突码对）
pub fn change_is_conflict(x: &str, y: &str) -> bool {
    matches!(
        (x, y),
        ("U", "U") | // UU 双方修改
        ("A", "A") | // AA 双方新增
        ("D", "D") | // DD 双方删除
        ("A", "U") | ("U", "A") | // AU / UA
        ("D", "U") | ("U", "D") // DU / UD
    )
}

/// 冲突文件的三个版本内容
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConflictVersions {
    /// 共同祖先版本（双方都新增时为 None）
    pub base: Option<String>,
    /// 本地版本（stage 2）
    pub ours: String,
    /// 远程版本（stage 3）
    pub theirs: String,
}

/// 项目当前全部冲突文件路径
pub fn conflicted_files(project_id: &str) -> AppResult<Vec<String>> {
    let st = status(project_id)?;
    Ok(st
        .changes
        .into_iter()
        .filter(|c| change_is_conflict(&c.x, &c.y))
        .map(|c| c.path)
        .collect())
}

/// 读取冲突文件的 base / ours / theirs 三方内容
pub fn conflict_versions(project_id: &str, path: &str) -> AppResult<ConflictVersions> {
    let root = repo_root(project_id)?;
    safe_join(&root, path)?;
    let stage = |n: u32| -> Option<String> {
        let out = run_git(&root, &["show", &format!(":{}:{}", n, path)]).ok()?;
        Some(crate::util::decode_output(&out.stdout))
    };
    Ok(ConflictVersions {
        base: stage(1),
        ours: stage(2).unwrap_or_default(),
        theirs: stage(3).unwrap_or_default(),
    })
}

/// 快捷采用某一侧并标记已解决：
/// - ours：`git checkout --ours` 后 add
/// - theirs:`git checkout --theirs` 后 add
/// - both：先本地后远程拼接写入后 add（IDEA 的「采用双侧」语义）
pub fn resolve_side(project_id: &str, path: &str, side: &str) -> AppResult<()> {
    let root = repo_root(project_id)?;
    safe_join(&root, path)?;
    match side {
        "ours" => {
            run_git(&root, &["checkout", "--ours", "--", path])?;
        }
        "theirs" => {
            run_git(&root, &["checkout", "--theirs", "--", path])?;
        }
        "both" => {
            let versions = conflict_versions(project_id, path)?;
            let mut merged = versions.ours;
            if !merged.ends_with('\n') && !merged.is_empty() {
                merged.push('\n');
            }
            merged.push_str(&versions.theirs);
            write_file(project_id, path, &merged)?;
        }
        _ => return Err(AppError::Git(format!("未知的解决方向: {}", side))),
    }
    stage(project_id, &[path.to_string()])
}

/// 标记冲突已解决（前端编辑完中间栏结果后调用）：写回内容 + git add
pub fn mark_resolved(project_id: &str, path: &str, content: &str) -> AppResult<()> {
    write_file(project_id, path, content)?;
    let root = repo_root(project_id)?;
    safe_join(&root, path)?;
    run_git(&root, &["add", "--", path])?;
    Ok(())
}

/// 全部冲突解决后提交完成合并：
/// 有自定义信息用 `-m`，否则 `--no-edit` 沿用 git 生成的默认合并信息
pub fn complete_merge(project_id: &str, message: Option<&str>) -> AppResult<()> {
    let root = repo_root(project_id)?;
    if !merge_in_progress(&root) {
        return Err(AppError::Git("当前没有进行中的合并".to_string()));
    }
    match message.map(str::trim).filter(|m| !m.is_empty()) {
        Some(m) => {
            run_git(&root, &["commit", "-m", m])?;
        }
        None => {
            run_git(&root, &["commit", "--no-edit"])?;
        }
    }
    Ok(())
}

/// 中止本次合并，工作区回到合并前状态
pub fn abort_merge(project_id: &str) -> AppResult<()> {
    let root = repo_root(project_id)?;
    if !merge_in_progress(&root) {
        return Err(AppError::Git("当前没有进行中的合并".to_string()));
    }
    run_git(&root, &["merge", "--abort"])?;
    Ok(())
}
