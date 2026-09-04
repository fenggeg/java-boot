//! Git CLI 执行层 + 纯函数解析器。
//!
//! 安全模型：
//! - 所有 git 调用都经 `GitRunner` 统一封装，禁止在各 command 里散落 `Command::new`；
//! - 参数一律用 `arg()` 逐个传递 + `current_dir(project_root)`，禁止拼接 shell 字符串；
//! - 文件路径参数一律放在 `--` 分隔符之后，防止路径被解析为选项（如名为 `-rf` 的文件）；
//! - 固定前缀 `--no-optional-locks`：防止 status 等命令 Opportunistic 刷新 index，
//!   既避免与用户自己的 git 操作抢锁，也避免写入 index 触发文件监听死循环；
//! - `-c core.quotepath=false`：中文等非 ASCII 路径不被八进制转义；
//! - `GIT_TERMINAL_PROMPT=0`：防止未来的网络类操作挂起进程；
//! - diff 类命令追加 `--no-color --no-ext-diff --no-textconv`：用户配置了外部 diff
//!   工具或 textconv 时，默认输出不可解析，必须显式关闭。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use once_cell::sync::OnceCell;
use parking_lot::{Condvar, Mutex};
use serde::Serialize;

use crate::util::{canonicalize_clean, NoWindow};

pub type GitResult<T> = Result<T, String>;

/// git 子进程并发上限：防止保存风暴/多文件并发刷新时进程堆积
const GIT_MAX_CONCURRENT: u32 = 2;

// ---- 轻量计数信号量（跨线程可用；tokio 的 Semaphore 是 async，无法在
//      spawn_blocking 的同步线程里直接 acquire） ----
static GIT_ACTIVE: Mutex<u32> = Mutex::new(0);
static GIT_COND: Condvar = Condvar::new();

struct GitSlot;
impl Drop for GitSlot {
    fn drop(&mut self) {
        let mut n = GIT_ACTIVE.lock();
        *n -= 1;
        GIT_COND.notify_one();
    }
}

fn acquire_git_slot() -> GitSlot {
    let mut n = GIT_ACTIVE.lock();
    while *n >= GIT_MAX_CONCURRENT {
        GIT_COND.wait(&mut n);
    }
    *n += 1;
    GitSlot
}

// ================================================================
// 数据契约（serde camelCase；与前端 TS 类型解耦）
// ================================================================

/// 单个 hunk（行号 1-based；0 表示「不存在/间隙之前的那一行」）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
}

/// 单文件 diff 结果（gutter 数据源）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    pub path: String,
    /// added / modified / deleted / renamed / unmodified / binary
    pub status: String,
    pub is_binary: bool,
    pub hunks: Vec<Hunk>,
}

/// git status 条目（-z 解析）
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileStatusEntry {
    pub path: String,
    /// rename 条目的原路径（-z 双字段），非 rename 为 null
    pub old_path: Option<String>,
    /// untracked / added / modified / deleted / renamed / copied / unmodified
    pub status: String,
    pub staged: bool,
}

/// blame 单行归属
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlameLine {
    pub sha: String,
    pub final_line: u32,
    pub author: String,
    /// epoch 秒
    pub time: i64,
    pub summary: String,
}

/// git 可用性探测结果（真实仓库根由 rev-parse --show-toplevel 解析）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitAvailability {
    pub installed: bool,
    pub is_repo: bool,
    pub repo_root: Option<String>,
}

// ================================================================
// GitRunner：统一执行层
// ================================================================

pub struct GitRunner {
    /// 规范化的项目根（command 传参的基准，工作目录）
    project_root: PathBuf,
    /// rev-parse --show-toplevel 解析出的真实仓库根（惰性缓存）
    repo_root: Option<PathBuf>,
}

impl GitRunner {
    pub fn new(project_root: PathBuf) -> Self {
        Self {
            project_root,
            repo_root: None,
        }
    }

    /// 解析真实仓库根（子目录仓库 / submodule / worktree 场景，仓库根可能
    /// 高于 project_root），非仓库返回 Err。结果缓存，避免重复调用。
    pub fn toplevel(&mut self) -> GitResult<&PathBuf> {
        if self.repo_root.is_none() {
            let out = self.run_bytes(&["rev-parse", "--show-toplevel"])?;
            let p = String::from_utf8_lossy(&out);
            let p = p.trim();
            if p.is_empty() {
                return Err("非 git 仓库".to_string());
            }
            // rev-parse 输出的是 git 视角路径，规范化以对齐 canonicalize 后的 project_root
            self.repo_root = Some(
                canonicalize_clean(Path::new(p)).unwrap_or_else(|| PathBuf::from(p)),
            );
        }
        Ok(self.repo_root.as_ref().unwrap())
    }

    /// 执行 git，成功返回 stdout（UTF-8 严格）
    pub fn run(&mut self, args: &[&str]) -> GitResult<String> {
        let bytes = self.run_bytes(args)?;
        String::from_utf8(bytes).map_err(|_| "git 输出非 UTF-8".to_string())
    }

    /// 同上，返回原始字节（供 cat-file 取 blob 内容 / status -z 解析）
    pub fn run_bytes(&mut self, args: &[&str]) -> GitResult<Vec<u8>> {
        let _slot = acquire_git_slot();
        let mut cmd = self.base();
        for a in args {
            cmd.arg(a);
        }
        let out = cmd.output().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "git 未安装或不在 PATH 中".to_string()
            } else {
                format!("执行 git 失败: {}", e)
            }
        })?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            // 错误分支只依据退出码判断；stderr 仅作展示，不参与控制流（可能被本地化）
            return Err(format!(
                "git 退出码 {:?}: {}",
                out.status.code(),
                stderr.trim()
            ));
        }
        Ok(out.stdout)
    }

    /// 构造基础 command：git + 固定前缀 + 工作目录 + 环境变量
    fn base(&self) -> Command {
        let mut c = Command::new("git");
        // 固定前缀（必须在子命令之前）
        c.arg("--no-optional-locks")
            .arg("-c")
            .arg("core.quotepath=false");
        c.current_dir(&self.project_root);
        c.env("GIT_TERMINAL_PROMPT", "0");
        c.stdin(Stdio::null());
        c.creation_flags_no_window();
        c
    }
}

/// git 是否已安装（--version 探测结果缓存；一次启动只探测一次）
static GIT_VERSION_CACHE: OnceCell<bool> = OnceCell::new();

/// 可用性探测：git 安装与否 + project_root 是否为 git 仓库，并返回真实仓库根
pub fn availability(project_root: &Path) -> GitAvailability {
    let installed = is_git_installed();
    if !installed {
        return GitAvailability {
            installed: false,
            is_repo: false,
            repo_root: None,
        };
    }
    let mut r = GitRunner::new(project_root.to_path_buf());
    match r.toplevel() {
        Ok(root) => GitAvailability {
            installed: true,
            is_repo: true,
            repo_root: Some(root.to_string_lossy().into_owned()),
        },
        Err(_) => GitAvailability {
            installed: true,
            is_repo: false,
            repo_root: None,
        },
    }
}

/// git 是否已安装（--version 探测结果缓存；一次启动只探测一次）
pub fn is_git_installed() -> bool {
    *GIT_VERSION_CACHE.get_or_init(|| {
        let _slot = acquire_git_slot();
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

/// 解析真实 git 目录（`--absolute-git-dir`；worktree/submodule 时 `.git` 是文件，
/// 真实 git 目录在别处，监听需指向它）。非仓库返回 None。
pub fn git_dir(project_root: &Path) -> Option<PathBuf> {
    let mut r = GitRunner::new(project_root.to_path_buf());
    let out = r.run_bytes(&["rev-parse", "--absolute-git-dir"]).ok()?;
    let p = String::from_utf8_lossy(&out);
    let p = p.trim();
    if p.is_empty() {
        return None;
    }
    Some(PathBuf::from(p))
}

/// 全仓库 status（-z 输出，NUL 分隔，rename 双字段）
pub fn status_all(project_root: &Path) -> GitResult<Vec<FileStatusEntry>> {
    let mut r = GitRunner::new(project_root.to_path_buf());
    r.toplevel()?; // 先校验是仓库
    let out = r.run_bytes(&[
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
    ])?;
    Ok(parse_status_z(&out))
}

/// 单文件 diff（gutter 数据源），按文件状态决定策略
pub fn file_diff(project_root: &Path, file_path: &str) -> GitResult<FileDiff> {
    let mut r = GitRunner::new(project_root.to_path_buf());
    let repo_root = r.toplevel()?.clone();
    let rel = rel_of(project_root, &repo_root, file_path)?;

    // 该文件状态：scoped status（git status 先在全树做 rename 检测再按 pathspec
    // 过滤，因此 `-- <新路径>` 仍能返回 R + 原路径）
    let st = r.run_bytes(&[
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
        "--",
        &rel,
    ])?;
    let entry = parse_status_z(&st)
        .into_iter()
        .find(|e| e.path == rel || e.old_path.as_deref() == Some(&rel));

    // 未出现在 status 里 → 工作区与 HEAD 一致（无改动）
    let entry = match entry {
        Some(e) => e,
        None => {
            return Ok(FileDiff {
                path: file_path.to_string(),
                status: "unmodified".to_string(),
                is_binary: false,
                hunks: vec![],
            });
        }
    };

    let status = entry.status.as_str();
    let abs = project_root.join(file_path);
    match status {
        // untracked（??）或 index 新增（A，不在 HEAD）→ 不跑 diff，
        // 读工作区文件行数，合成整文件 hunk（全部为新增）
        "untracked" | "added" => {
            let lines = count_lines(&abs);
            Ok(FileDiff {
                path: file_path.to_string(),
                status: "added".to_string(),
                is_binary: false,
                hunks: vec![Hunk {
                    old_start: 0,
                    old_lines: 0,
                    new_start: 1,
                    new_lines: lines,
                }],
            })
        }
        // rename：diff 必须同时传原路径与新路径。只传新路径会导致 rename 检测
        // 失效，整文件被误判为新增
        "renamed" => {
            let old = entry
                .old_path
                .ok_or_else(|| "rename 条目缺少原路径".to_string())?;
            let out = r.run_bytes(&[
                "diff",
                "HEAD",
                "-M",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "-U0",
                "--",
                &old,
                &rel,
            ])?;
            Ok(build_diff(file_path, "renamed", &out))
        }
        // 其他（modified / deleted）→ diff HEAD（= 暂存区+工作区 相对 HEAD）
        _ => {
            let out = r.run_bytes(&[
                "diff",
                "HEAD",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                // -U0：默认 3 行上下文会撑大 hunk 范围，gutter 标记会不准
                "-U0",
                "-M",
                "--",
                &rel,
            ])?;
            Ok(build_diff(file_path, status, &out))
        }
    }
}

/// 构建 FileDiff：检测二进制、解析 hunk、推导状态
fn build_diff(path: &str, raw_status: &str, out: &[u8]) -> FileDiff {
    if has_binary_line(&String::from_utf8_lossy(out)) {
        return FileDiff {
            path: path.to_string(),
            status: "binary".to_string(),
            is_binary: true,
            hunks: vec![],
        };
    }
    let text = String::from_utf8_lossy(out);
    let hunks = parse_hunk_headers(&text);
    let status = match raw_status {
        "renamed" => "renamed".to_string(),
        "deleted" => "deleted".to_string(),
        "added" | "untracked" => "added".to_string(),
        _ => {
            if hunks.is_empty() {
                "unmodified".to_string()
            } else {
                "modified".to_string()
            }
        }
    };
    FileDiff {
        path: path.to_string(),
        status,
        is_binary: false,
        hunks,
    }
}

/// HEAD 中该文件的原始内容（新文件不在 HEAD 中 → None；非 UTF-8 → None）
pub fn file_at_head(project_root: &Path, file_path: &str) -> GitResult<Option<String>> {
    let mut r = GitRunner::new(project_root.to_path_buf());
    let repo_root = r.toplevel()?.clone();
    let rel = rel_of(project_root, &repo_root, file_path)?;
    let arg = format!("HEAD:{}", rel);
    match r.run_bytes(&["cat-file", "-p", &arg]) {
        Ok(bytes) => Ok(String::from_utf8(bytes).ok()),
        Err(_) => Ok(None), // 新文件不在 HEAD / 其它失败一律视为无历史版本
    }
}

/// blame（--porcelain 输出按 final 行号归并）
pub fn blame(project_root: &Path, file_path: &str) -> GitResult<Vec<BlameLine>> {
    let mut r = GitRunner::new(project_root.to_path_buf());
    let repo_root = r.toplevel()?.clone();
    let rel = rel_of(project_root, &repo_root, file_path)?;
    let out = r.run_bytes(&["blame", "--porcelain", "--", &rel])?;
    Ok(parse_blame_porcelain(&String::from_utf8_lossy(&out)))
}

// ================================================================
// 路径安全校验
// ================================================================

/// 把「项目相对路径」换算为「仓库根相对路径」（正斜杠），并做安全校验：
/// 拒绝绝对路径、`..` 越界、仓库外路径。
///
/// 注意：不拒绝 `-` 开头的路径——真实文件名可能叫 `-rf`，且所有路径参数
/// 在调用处一律放在 `--` 分隔符之后，git 会将其视为 pathspec 而非选项（双保险）。
pub fn rel_of(project_root: &Path, repo_root: &Path, file_path: &str) -> GitResult<String> {
    if file_path.is_empty() {
        return Err("文件路径为空".to_string());
    }
    if file_path.starts_with('/') || file_path.starts_with("..") {
        return Err(format!("非法的文件路径: {}", file_path));
    }
    if file_path
        .split(['/', '\\'])
        .any(|seg| seg == ".." || seg.is_empty())
    {
        return Err(format!("非法的文件路径: {}", file_path));
    }
    let abs = project_root.join(file_path);
    // 优先 canonicalize（解析大小写/符号链接），文件已删除（如 git 删除）时退回比较
    let canon = canonicalize_clean(&abs).unwrap_or_else(|| abs.clone());
    let repo = canonicalize_clean(repo_root).unwrap_or_else(|| repo_root.to_path_buf());
    let rel = strip_prefix_ci(&canon, &repo)?;
    Ok(rel.replace('\\', "/"))
}

/// strip_prefix，Windows 上大小写不敏感兜底（git 路径大小写与磁盘可能不一致）
fn strip_prefix_ci(path: &Path, base: &Path) -> GitResult<String> {
    if let Ok(r) = path.strip_prefix(base) {
        return Ok(r.to_string_lossy().into_owned());
    }
    #[cfg(windows)]
    {
        let a = path.to_string_lossy();
        let b = base.to_string_lossy();
        if a.len() > b.len() && a[..b.len()].eq_ignore_ascii_case(&b) {
            let rest = a[b.len()..].trim_start_matches(['\\', '/']);
            if !rest.is_empty() {
                return Ok(rest.to_string());
            }
        }
    }
    Err(format!(
        "文件不在 git 仓库内: {}",
        path.to_string_lossy()
    ))
}

/// 统计工作区文件行数（LF/CRLF 均按 `\n` 计；末行无换行也计 1 行）
fn count_lines(path: &Path) -> u32 {
    std::fs::read(path)
        .map(|b| {
            let n = b.iter().filter(|&&c| c == b'\n').count() as u32;
            if !b.is_empty() && b.last() != Some(&b'\n') {
                n + 1
            } else {
                n
            }
        })
        .unwrap_or(0)
}

// ================================================================
// 纯函数解析器（无 IO，可单测）
// ================================================================

/// 只解析 `@@` 开头的 hunk 头：`@@ -oldStart[,oldLines] +newStart[,newLines] @@`
/// - 计数省略时视为 1；
/// - 计数为 0 时 start 表示「间隙之前的那一行」（关键语义：纯删除标记画在
///   newStart+1，纯新增标记范围从 newStart 开始）
pub fn parse_hunk_headers(output: &str) -> Vec<Hunk> {
    let mut out = Vec::new();
    for line in output.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line); // 容忍 CRLF
        let line = line.trim();
        if !line.starts_with("@@") {
            continue;
        }
        if let Some(h) = parse_hunk_header(line) {
            out.push(h);
        }
    }
    out
}

fn parse_hunk_header(line: &str) -> Option<Hunk> {
    let body = line.strip_prefix("@@")?.trim();
    let mut it = body.splitn(2, ' ');
    let old_tok = it.next()?.trim();
    let new_tok = it.next()?.trim();
    if !old_tok.starts_with('-') || !new_tok.starts_with('+') {
        return None;
    }
    // 尾部可能带 `@@` 与函数上下文，取首个空白分隔 token 即可
    let old_rng = old_tok[1..].split_whitespace().next().unwrap_or("");
    let new_rng = new_tok[1..].split_whitespace().next().unwrap_or("");
    let (old_start, old_lines) = parse_range(old_rng);
    let (new_start, new_lines) = parse_range(new_rng);
    Some(Hunk {
        old_start,
        old_lines,
        new_start,
        new_lines,
    })
}

/// 解析 `start[,lines]`；计数省略视为 1
fn parse_range(s: &str) -> (u32, u32) {
    if let Some((a, b)) = s.split_once(',') {
        (a.parse().unwrap_or(1), b.parse().unwrap_or(1))
    } else {
        (s.parse().unwrap_or(1), 1)
    }
}

/// 解析 `git status --porcelain=v1 -z` 的 NUL 分隔输出。
/// 条目格式 `XY <path>`；rename（R/C）条目后跟第二个 NUL 分隔字段为原路径。
/// `??` 映射为 status="untracked"、staged=false。
pub fn parse_status_z(data: &[u8]) -> Vec<FileStatusEntry> {
    let mut out = Vec::new();
    let records: Vec<&[u8]> = data.split(|&b| b == 0).collect();
    let mut i = 0;
    while i < records.len() {
        let rec = records[i];
        if rec.len() < 3 {
            i += 1;
            continue;
        }
        let x = rec[0] as char;
        let y = rec[1] as char;
        // 路径可能含空格，取前两个状态字符 + 空格后的整体
        let path = String::from_utf8_lossy(&rec[3..]).into_owned();
        let mut old_path: Option<String> = None;
        if x == 'R' || y == 'R' || x == 'C' || y == 'C' {
            if let Some(next) = records.get(i + 1) {
                if !next.is_empty() {
                    old_path = Some(String::from_utf8_lossy(next).into_owned());
                    i += 1; // 消费原路径字段
                }
            }
        }
        out.push(FileStatusEntry {
            path,
            old_path,
            status: status_of(x, y).0.to_string(),
            staged: status_of(x, y).1,
        });
        i += 1;
    }
    out
}

/// XY → (status, staged)。X 为暂存区状态，Y 为工作区状态
fn status_of(x: char, y: char) -> (&'static str, bool) {
    match (x, y) {
        ('?', '?') => ("untracked", false),
        ('A', _) => ("added", true),
        ('R', _) => ("renamed", true),
        ('C', _) => ("copied", true),
        ('M', _) => ("modified", true),
        ('D', _) => ("deleted", true),
        (_, 'M') => ("modified", false),
        (_, 'D') => ("deleted", false),
        _ => ("unmodified", false),
    }
}

/// 是否命中 `Binary files ... differ`（容忍 CRLF / 首尾空白）
pub fn has_binary_line(output: &str) -> bool {
    output
        .lines()
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .any(|l| l.trim_start().starts_with("Binary files") && l.contains("differ"))
}

/// 解析 `git blame --porcelain`：按 final 行号归并出每条行的归属。
/// 每组连续同 commit 的行仅首行带 author / author-time / summary 元数据。
pub fn parse_blame_porcelain(output: &str) -> Vec<BlameLine> {
    #[derive(Default)]
    struct Meta {
        sha: String,
        author: String,
        time: i64,
        summary: String,
    }

    let mut result: Vec<BlameLine> = Vec::new();
    let mut meta = Meta::default();
    let mut pending_final: u32 = 0;
    let mut has_header = false;

    for line in output.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        if let Some(content) = line.strip_prefix('\t') {
            // 内容行：用当前组的元数据落一条 BlameLine
            if has_header {
                result.push(BlameLine {
                    sha: meta.sha.clone(),
                    final_line: pending_final,
                    author: meta.author.clone(),
                    time: meta.time,
                    summary: meta.summary.clone(),
                });
                let _ = content;
            }
            continue;
        }
        // 头部行
        let first = line.split_whitespace().next().unwrap_or("");
        if is_sha_token(first) {
            let mut it = line.split_whitespace();
            let sha = it.next().unwrap_or("").to_string();
            let _orig: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(1);
            let final_line: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(1);
            // 新组（sha 变化）才重置元数据；同组连续行复用
            if meta.sha != sha {
                meta = Meta {
                    sha,
                    author: String::new(),
                    time: 0,
                    summary: String::new(),
                };
            }
            pending_final = final_line;
            has_header = true;
        } else if has_header {
            // 元数据行：仅首行出现
            if let Some(v) = line.strip_prefix("author ") {
                meta.author = v.trim().to_string();
            } else if let Some(v) = line.strip_prefix("author-time ") {
                meta.time = v.trim().parse().unwrap_or(0);
            } else if let Some(v) = line.strip_prefix("summary ") {
                meta.summary = v.trim().to_string();
            }
        }
    }

    result.sort_by_key(|b| b.final_line);
    result
}

/// SHA-1/256 token 判定（至少 7 位十六进制）
fn is_sha_token(s: &str) -> bool {
    s.len() >= 7 && s.chars().all(|c| c.is_ascii_hexdigit())
}

// ================================================================
// 单元测试：解析器必须是纯函数，覆盖规范要求的样例
// ================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- hunk 头 ----------

    #[test]
    fn hunk_standard_format() {
        let out = "@@ -5,3 +5,3 @@ fn foo()\n";
        let h = parse_hunk_headers(out);
        assert_eq!(
            h,
            vec![Hunk {
                old_start: 5,
                old_lines: 3,
                new_start: 5,
                new_lines: 3,
            }]
        );
    }

    #[test]
    fn hunk_omitted_counts() {
        // 计数省略视为 1
        let h = parse_hunk_headers("@@ -5 +5 @@\n");
        assert_eq!(
            h,
            vec![Hunk {
                old_start: 5,
                old_lines: 1,
                new_start: 5,
                new_lines: 1,
            }]
        );
    }

    #[test]
    fn hunk_pure_deletion() {
        // 纯删除：newLines==0，newStart 是间隙前一行
        let h = parse_hunk_headers("@@ -5,3 +4,0 @@\n");
        assert_eq!(
            h,
            vec![Hunk {
                old_start: 5,
                old_lines: 3,
                new_start: 4,
                new_lines: 0,
            }]
        );
    }

    #[test]
    fn hunk_pure_addition() {
        // 纯新增：oldLines==0
        let h = parse_hunk_headers("@@ -4,0 +5,3 @@\n");
        assert_eq!(
            h,
            vec![Hunk {
                old_start: 4,
                old_lines: 0,
                new_start: 5,
                new_lines: 3,
            }]
        );
    }

    #[test]
    fn hunk_multiple_and_ignored_lines() {
        let out = "\
index 1234567..89abcde 100644
--- a/src/a.java
+++ b/src/a.java
@@ -1,2 +1,2 @@
-context
+changed
@@ -10 +11,2 @@
 more
+add
";
        let h = parse_hunk_headers(out);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0], Hunk { old_start: 1, old_lines: 2, new_start: 1, new_lines: 2 });
        assert_eq!(h[1], Hunk { old_start: 10, old_lines: 1, new_start: 11, new_lines: 2 });
    }

    #[test]
    fn hunk_crlf_tolerated() {
        let h = parse_hunk_headers("@@ -5,3 +5,3 @@\r\n");
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].old_start, 5);
    }

    #[test]
    fn hunk_empty_output() {
        assert!(parse_hunk_headers("").is_empty());
        assert!(parse_hunk_headers("no hunks here\njust text\n").is_empty());
    }

    // ---------- status -z ----------

    #[test]
    fn status_z_rename_double_field() {
        // rename：R 后跟第二个 NUL 字段为原路径
        let mut data = Vec::new();
        data.extend_from_slice(b"R  new/file.java");
        data.push(0);
        data.extend_from_slice(b"old/file.java");
        data.push(0);
        let entries = parse_status_z(&data);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "new/file.java");
        assert_eq!(entries[0].old_path.as_deref(), Some("old/file.java"));
        assert_eq!(entries[0].status, "renamed");
        assert!(entries[0].staged);
    }

    #[test]
    fn status_z_untracked() {
        let mut data = Vec::new();
        data.extend_from_slice(b"?? newfile.txt");
        data.push(0);
        let entries = parse_status_z(&data);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, "untracked");
        assert!(!entries[0].staged);
    }

    #[test]
    fn status_z_spaces_and_chinese() {
        // 含空格与中文路径（core.quotepath=false 下原样 UTF-8 输出）
        let mut data = Vec::new();
        data.extend_from_slice(" M 我的 文件.java".as_bytes());
        data.push(0);
        let entries = parse_status_z(&data);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "我的 文件.java");
        assert_eq!(entries[0].status, "modified");
        assert!(!entries[0].staged);
    }

    #[test]
    fn status_z_multiple_kinds() {
        let mut data = Vec::new();
        data.extend_from_slice(b"A  staged.txt");
        data.push(0);
        data.extend_from_slice(b" D worktree-deleted.txt");
        data.push(0);
        data.extend_from_slice(b"?? untracked.bin");
        data.push(0);
        let entries = parse_status_z(&data);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].status, "added");
        assert_eq!(entries[1].status, "deleted");
        assert_eq!(entries[2].status, "untracked");
    }

    // ---------- Binary / 空 diff ----------

    #[test]
    fn binary_files_line() {
        assert!(has_binary_line("Binary files a/x.jar and b/x.jar differ\n"));
        assert!(has_binary_line("Binary files a/x.jar and b/x.jar differ\r\n"));
        assert!(!has_binary_line(""));
        assert!(!has_binary_line("just a normal diff\n@@ -1 +1 @@\n"));
    }

    // ---------- blame porcelain ----------

    #[test]
    fn blame_porcelain_basic() {
        // 一组 2 行（同一 commit）+ 一组 1 行（另一 commit），仅首行带元数据
        let out = "\
f00d1234567890123456789012345678901234567 1 1 2
author Alice
author-mail <alice@example.com>
author-time 1700000000
author-tz +0800
committer Alice
committer-mail <alice@example.com>
committer-time 1700000000
committer-tz +0800
summary Add feature X
filename src/A.java
\tline one
f00d1234567890123456789012345678901234567 2 2
\tline two
beef4567890123456789012345678901234567890 3 3
author Bob
author-mail <bob@example.com>
author-time 1710000000
author-tz +0800
committer Bob
committer-mail <bob@example.com>
committer-time 1710000000
committer-tz +0800
summary Fix bug Y
filename src/A.java
\tline three
";
        let lines = parse_blame_porcelain(out);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].final_line, 1);
        assert_eq!(lines[0].author, "Alice");
        assert_eq!(lines[0].summary, "Add feature X");
        assert_eq!(lines[0].time, 1700000000);
        assert_eq!(lines[1].final_line, 2);
        // 同组后续行复用首行元数据
        assert_eq!(lines[1].author, "Alice");
        assert_eq!(lines[2].final_line, 3);
        assert_eq!(lines[2].author, "Bob");
        assert_eq!(lines[2].summary, "Fix bug Y");
    }

    // ---------- 路径换算 ----------

    #[test]
    fn rel_of_rejects_bad_paths() {
        let root = Path::new("C:/proj");
        let repo = Path::new("C:/proj");
        assert!(rel_of(root, repo, "../escape.txt").is_err());
        assert!(rel_of(root, repo, "/etc/passwd").is_err());
        assert!(rel_of(root, repo, "a/../b.txt").is_err());
        assert!(rel_of(root, repo, "a//b.txt").is_err());
        assert!(rel_of(root, repo, "").is_err());
        assert_eq!(rel_of(root, repo, "src/main.java").unwrap(), "src/main.java");
        // `-` 开头的真实文件名：不拒绝，交给 `--` 分隔符保护
        assert_eq!(rel_of(root, repo, "-rf").unwrap(), "-rf");
        // 含空格 / 中文 / glob 特殊字符
        assert_eq!(rel_of(root, repo, "我的 文件*.java").unwrap(), "我的 文件*.java");
    }

    #[test]
    fn count_lines_samples() {
        assert_eq!(count_lines(Path::new("__nonexistent__")), 0);
        // 末行无换行也计 1 行
        let f = std::env::temp_dir().join("jb_cnt_nl.txt");
        std::fs::write(&f, "a\nb").unwrap();
        assert_eq!(count_lines(&f), 2);
        std::fs::write(&f, "a\nb\n").unwrap();
        assert_eq!(count_lines(&f), 2);
        std::fs::write(&f, "").unwrap();
        assert_eq!(count_lines(&f), 0);
        std::fs::write(&f, "a\r\nb\r\n").unwrap();
        assert_eq!(count_lines(&f), 2);
        let _ = std::fs::remove_file(&f);
    }

    // ================================================================
    // 真实 git 集成测试（机器装有 git 时执行；未安装则跳过）
    // 证明 -z 与 `--` 分隔符对「空格 / 中文 / - 开头」路径的保护
    // ================================================================

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn init_repo() -> Option<PathBuf> {
        if !git_available() {
            return None;
        }
        let dir = std::env::temp_dir().join(format!("jb_git_integ_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok()?;
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&dir)
                .stdin(Stdio::null())
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !(run(&["init", "-q"])
            && run(&["config", "user.email", "test@example.com"])
            && run(&["config", "user.name", "Tester"]))
        {
            let _ = std::fs::remove_dir_all(&dir);
            return None;
        }
        Some(dir)
    }

    fn git_run(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn integration_diff_status_head_blame() {
        let Some(dir) = init_repo() else { return };
        // 首次提交
        std::fs::write(dir.join("a.txt"), "line1\nline2\nline3\n").unwrap();
        assert!(git_run(&dir, &["add", "a.txt"]));
        assert!(git_run(&dir, &["commit", "-q", "-m", "init"]));
        // blame：在文件修改前执行（blame 默认针对工作区，含未提交改动）
        let bl = blame(&dir, "a.txt").unwrap();
        assert_eq!(bl.len(), 3);
        assert!(bl.iter().all(|b| b.author == "Tester" && b.summary == "init"));
        // 修改一行 → modified hunk（-U0 下为单行 hunk）
        std::fs::write(dir.join("a.txt"), "line1\nline2 changed\nline3\n").unwrap();
        let d = file_diff(&dir, "a.txt").unwrap();
        assert!(!d.is_binary);
        assert_eq!(d.status, "modified");
        assert_eq!(
            d.hunks,
            vec![Hunk {
                old_start: 2,
                old_lines: 1,
                new_start: 2,
                new_lines: 1,
            }]
        );
        // 含空格 + 中文的文件名 → untracked，合成整文件 hunk
        std::fs::write(dir.join("我的 文件.txt"), "x\n").unwrap();
        let d2 = file_diff(&dir, "我的 文件.txt").unwrap();
        assert_eq!(d2.status, "added");
        assert_eq!(
            d2.hunks,
            vec![Hunk {
                old_start: 0,
                old_lines: 0,
                new_start: 1,
                new_lines: 1,
            }]
        );
        // `-` 开头的真实文件名：`--` 分隔符保护下正常工作
        std::fs::write(dir.join("-rf"), "y\n").unwrap();
        let d3 = file_diff(&dir, "-rf").unwrap();
        assert_eq!(d3.status, "added");
        // HEAD 内容
        let head = file_at_head(&dir, "a.txt").unwrap();
        assert!(head.as_deref().unwrap_or("").contains("line2\n"));
        // status_all：三个条目都在
        let st = status_all(&dir).unwrap();
        assert!(st.iter().any(|e| e.path == "我的 文件.txt" && e.status == "untracked"));
        assert!(st.iter().any(|e| e.path == "-rf" && e.status == "untracked"));
        // 空仓库（无 HEAD）：所有文件按 added 处理
        let empty = std::env::temp_dir().join(format!("jb_git_empty_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&empty);
        std::fs::create_dir_all(&empty).unwrap();
        assert!(git_run(&empty, &["init", "-q"]));
        std::fs::write(empty.join("x.txt"), "a\nb\n").unwrap();
        let de = file_diff(&empty, "x.txt").unwrap();
        assert_eq!(de.status, "added");
        assert_eq!(de.hunks[0].new_lines, 2);
        let _ = std::fs::remove_dir_all(&empty);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
