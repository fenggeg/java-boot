//! 项目文件系统操作（文件浏览 / 预览 / 编辑）
//!
//! 基于 `project.root_path`，不依赖 Git 仓库（与 git.rs 的 read/write 区分）：
//! - `list_dir`：惰性列出单层目录，过滤构建产物/依赖等大目录
//! - `read_file`：UTF-8 优先；失败尝试 GBK 转码（中文项目常见），非 UTF-8 标记只读防写坏编码；超大文件只读预览
//! - `write_file`：仅写入 UTF-8 文本，路径校验防止越出项目根

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::db;
use crate::error::{AppError, AppResult};

/// 目录条目（单层）
#[derive(Debug, Clone, Serialize)]
pub struct FileEntry {
    pub name: String,
    /// 相对项目根的路径（`/` 分隔，空串为根目录）
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
}

/// 文件内容 + 可编辑性
#[derive(Debug, Clone, Serialize)]
pub struct FileContent {
    pub content: String,
    /// "utf-8" | "gbk" | "unknown"
    pub encoding: String,
    /// 非 UTF-8 或超大文件时只读（防止写坏编码 / 误改大文件）
    pub readonly: bool,
    pub size: u64,
}

/// 跳过的大目录（依赖/构建产物/版本控制等），避免树卡顿
const DIR_BLACKLIST: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".git",
    ".svn",
    ".hg",
    ".idea",
    ".gradle",
    ".cache",
    ".pytest_cache",
    ".next",
    ".nuxt",
    "__pycache__",
    ".venv",
    "venv",
    "coverage",
    ".terraform",
];

/// 可编辑文件大小上限（超过则只读预览）
const MAX_EDIT_SIZE: usize = 2 * 1024 * 1024;

/// 项目根目录下安全拼接相对路径：拒绝 `..` / 绝对路径 / 盘符前缀，
/// 并对结果做 canonicalize 校验，防止 symlink 越出项目根
fn safe_join(root: &Path, rel: &str) -> AppResult<PathBuf> {
    let trimmed = rel.trim_start_matches(['/', '\\']);
    let p = Path::new(trimmed);
    for c in p.components() {
        match c {
            Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(AppError::Other(format!("非法路径: {}", rel)));
            }
            _ => {}
        }
    }
    let joined = root.join(p);
    // canonicalize 校验：解析 symlink 后确认最终路径仍在项目根内
    // 对于还不存在的文件（write_file 场景），canonicalize 会失败，
    // 改为对 parent 目录校验
    let canonical_root = root.canonicalize().map_err(|e| {
        AppError::Other(format!("项目根目录无法解析: {}", e))
    })?;
    // 尝试 canonicalize 目标路径；若不存在则 canonicalize parent 后拼接文件名
    let canonical_target = match joined.canonicalize() {
        Ok(c) => c,
        Err(_) => {
            // 文件可能还不存在（写入场景），对父目录做 canonicalize
            let parent = joined.parent().unwrap_or(&canonical_root);
            let canonical_parent = parent.canonicalize().unwrap_or_else(|_| canonical_root.clone());
            canonical_parent.join(joined.file_name().unwrap_or_default())
        }
    };
    if !canonical_target.starts_with(&canonical_root) {
        return Err(AppError::Other(format!("路径越界: {}", rel)));
    }
    Ok(joined)
}

fn join_rel(rel: &str, name: &str) -> String {
    if rel.is_empty() {
        name.to_string()
    } else {
        format!("{}/{}", rel, name)
    }
}

/// 列出项目根下某目录（单层）。`path` 为空串表示根目录
pub fn list_dir(project_id: &str, path: &str) -> AppResult<Vec<FileEntry>> {
    let project = db::get_project(project_id)?;
    let dir = safe_join(Path::new(&project.root_path), path)?;
    if !dir.is_dir() {
        return Err(AppError::NotFound(format!("目录不存在: {}", dir.display())));
    }
    let mut entries: Vec<FileEntry> = vec![];
    for entry in std::fs::read_dir(&dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            continue; // 跳过符号链接，避免循环/越界
        }
        if ft.is_dir() {
            if DIR_BLACKLIST.contains(&name.as_str()) {
                continue;
            }
            entries.push(FileEntry {
                path: join_rel(path, &name),
                name,
                is_dir: true,
                size: 0,
            });
        } else if ft.is_file() {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            entries.push(FileEntry {
                path: join_rel(path, &name),
                name,
                is_dir: false,
                size,
            });
        }
    }
    // 目录优先，同类型按名称（忽略大小写）排序
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

/// 读取项目文件内容（UTF-8 / GBK 探测，2MB 内可编辑）
pub fn read_file(project_id: &str, path: &str) -> AppResult<FileContent> {
    let project = db::get_project(project_id)?;
    let full = safe_join(Path::new(&project.root_path), path)?;
    if !full.is_file() {
        return Err(AppError::NotFound(format!("文件不存在: {}", path)));
    }
    // 先检查文件大小，避免一次性读入超大文件导致内存耗尽
    let metadata = std::fs::metadata(&full)
        .map_err(|e| AppError::Other(format!("读取文件信息失败: {}", e)))?;
    let file_size = metadata.len();
    if file_size > (MAX_EDIT_SIZE as u64) * 2 {
        // 超过可编辑上限的 2 倍：直接拒绝，不读入内存
        return Err(AppError::Other(format!(
            "文件超过 {}MB，暂不支持读取",
            MAX_EDIT_SIZE * 2 / 1024 / 1024
        )));
    }
    let bytes = std::fs::read(&full)
        .map_err(|e| AppError::Other(format!("读取失败: {}", e)))?;
    let size = bytes.len() as u64;

    if bytes.len() > MAX_EDIT_SIZE {
        // 超大文件：仅当是 UTF-8 文本时只读预览，否则拒绝
        match String::from_utf8(bytes) {
            Ok(s) => Ok(FileContent {
                content: s,
                encoding: "utf-8".to_string(),
                readonly: true,
                size,
            }),
            Err(_) => Err(AppError::Other(format!(
                "文件超过 {}MB 或非文本，暂不支持",
                MAX_EDIT_SIZE / 1024 / 1024
            ))),
        }
    } else {
        match String::from_utf8(bytes) {
            Ok(s) => Ok(FileContent {
                content: s,
                encoding: "utf-8".to_string(),
                readonly: false,
                size,
            }),
            Err(e) => {
                // 尝试 GBK（Windows 中文环境常见于 properties / 旧 Java 文件）
                let (cow, _, had_errors) = encoding_rs::GBK.decode(e.as_bytes());
                if had_errors {
                    return Err(AppError::Other(
                        "文件不是 UTF-8/GBK 文本，暂不支持预览".to_string(),
                    ));
                }
                Ok(FileContent {
                    content: cow.into_owned(),
                    encoding: "gbk".to_string(),
                    readonly: true, // 非 UTF-8 只读，防止写回时破坏编码
                    size,
                })
            }
        }
    }
}

/// 写回项目文件（UTF-8）
pub fn write_file(project_id: &str, path: &str, content: &str) -> AppResult<()> {
    let project = db::get_project(project_id)?;
    let full = safe_join(Path::new(&project.root_path), path)?;
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AppError::Other(format!("创建目录失败: {}", e)))?;
    }
    std::fs::write(&full, content.as_bytes())
        .map_err(|e| AppError::Other(format!("写入失败: {}", e)))?;
    Ok(())
}

/// 获取文件的绝对路径（前端用于图片预览等）
pub fn get_file_abs_path(project_id: &str, path: &str) -> AppResult<String> {
    let project = db::get_project(project_id)?;
    let full = safe_join(Path::new(&project.root_path), path)?;
    if !full.exists() {
        return Err(AppError::NotFound(format!("文件不存在: {}", path)));
    }
    Ok(full.to_string_lossy().to_string())
}

// ================================================================
// 全量文件扁平遍历（前端快速打开 / 文件名搜索的数据源）
// ================================================================

/// 扁平文件条目：只含文件不含目录（快速打开按文件名跳转，目录无意义）
#[derive(Debug, Clone, Serialize)]
pub struct FlatFile {
    /// 相对项目根的路径（`/` 分隔）
    pub path: String,
    pub name: String,
}

/// 遍历条目上限：极端仓库防止拖爆内存与 IPC 传输
const MAX_WALK_FILES: usize = 50_000;
/// 目录深度上限：防御 Windows junction（file_type 不算 symlink）循环等异常树
const MAX_WALK_DEPTH: usize = 24;

/// walk_files 缓存：避免每次打开 Ctrl+P 弹层都全量遍历大型项目目录。
/// key = project_id, value = (文件列表, 缓存时刻)
/// TTL 5 秒：兼顾快速连续打开弹层的性能与 git 操作后的新鲜度
static WALK_CACHE: once_cell::sync::Lazy<std::sync::Mutex<HashMap<String, (Vec<FlatFile>, std::time::Instant)>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(HashMap::new()));
const WALK_CACHE_TTL: Duration = Duration::from_secs(5);

fn rel_depth(rel: &str) -> usize {
    if rel.is_empty() {
        0
    } else {
        rel.split('/').count()
    }
}

/// 递归列出项目内全部文件，扁平返回。读操作不经过 safe_join；
/// 无权限 / 已消失的目录静默跳过，达到上限即截断
fn walk_dir(dir: &Path, rel: &str, out: &mut Vec<FlatFile>) {
    if out.len() >= MAX_WALK_FILES {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        if out.len() >= MAX_WALK_FILES {
            return;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            continue; // 跳过符号链接，避免循环/越界（与 list_dir 口径一致）
        }
        if ft.is_dir() {
            if DIR_BLACKLIST.contains(&name.as_str()) {
                continue;
            }
            let child_rel = join_rel(rel, &name);
            if rel_depth(&child_rel) > MAX_WALK_DEPTH {
                continue;
            }
            walk_dir(&entry.path(), &child_rel, out);
        } else if ft.is_file() {
            out.push(FlatFile {
                path: join_rel(rel, &name),
                name,
            });
        }
    }
}

/// 遍历项目根下全部文件（排除黑名单目录与符号链接）
pub fn walk_files(project_id: &str) -> AppResult<Vec<FlatFile>> {
    // 检查缓存：TTL 内直接返回，避免重复全量遍历
    {
        let cache = WALK_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((files, ts)) = cache.get(project_id) {
            if ts.elapsed() < WALK_CACHE_TTL {
                return Ok(files.clone());
            }
        }
    }
    let project = db::get_project(project_id)?;
    let root = Path::new(&project.root_path)
        .canonicalize()
        .map_err(|e| AppError::Other(format!("项目根目录无法解析: {}", e)))?;
    let mut out = Vec::new();
    walk_dir(&root, "", &mut out);
    // 写入缓存
    {
        let mut cache = WALK_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        cache.insert(project_id.to_string(), (out.clone(), std::time::Instant::now()));
    }
    Ok(out)
}

// ================================================================
// 文件树右键菜单操作：重命名 / 复制 / 移动 / 在文件管理器中显示
// ================================================================

/// 校验新文件名：禁止路径分隔符与 Windows 非法字符，禁止保留名
fn validate_name(name: &str) -> AppResult<()> {
    let n = name.trim();
    if n.is_empty() || n == "." || n == ".." {
        return Err(AppError::Other("无效的名称".into()));
    }
    if n.contains('/') || n.contains('\\') || n.contains(':') {
        return Err(AppError::Other(format!("名称不能包含分隔符: {}", n)));
    }
    for c in ['<', '>', '"', '|', '?', '*'] {
        if n.contains(c) {
            return Err(AppError::Other(format!("名称含非法字符: {}", c)));
        }
    }
    // Windows 保留设备名（CON/PRN/AUX/NUL/COM1-9/LPT1-9）
    let stem = n.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem[3..].bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(AppError::Other(format!("Windows 保留名不可用: {}", n)));
    }
    Ok(())
}

/// 目标重名时生成不冲突名称："a.txt" → "a (2).txt"，目录 → "dir (2)"
fn unique_name(parent: &Path, name: &str) -> String {
    if !parent.join(name).exists() {
        return name.to_string();
    }
    let stem_os = Path::new(name).file_stem().map(|s| s.to_string_lossy().to_string());
    let ext = Path::new(name).extension().map(|e| e.to_string_lossy().to_string());
    let stem = stem_os.unwrap_or_else(|| name.to_string());
    for i in 2..10000u32 {
        let candidate = match &ext {
            Some(e) => format!("{} ({}).{}", stem, i, e),
            None => format!("{} ({})", stem, i),
        };
        if !parent.join(&candidate).exists() {
            return candidate;
        }
    }
    // 极端兜底：加 uuid 后缀
    format!("{}-{}", stem, uuid::Uuid::new_v4().simple())
}

/// 重命名项目内文件 / 目录，返回新相对路径
pub fn rename_entry(project_id: &str, path: &str, new_name: &str) -> AppResult<String> {
    validate_name(new_name)?;
    let project = db::get_project(project_id)?;
    let root = Path::new(&project.root_path);
    let full = safe_join(root, path)?;
    if !full.exists() {
        return Err(AppError::NotFound(format!("文件不存在: {}", path)));
    }
    let parent = full.parent().ok_or_else(|| AppError::Other("无法获取父目录".into()))?;
    let target = parent.join(new_name);
    if target.exists() {
        return Err(AppError::Other(format!("同名条目已存在: {}", new_name)));
    }
    std::fs::rename(&full, &target).map_err(|e| AppError::Other(format!("重命名失败: {}", e)))?;
    let new_rel = join_rel(
        Path::new(path)
            .parent()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default()
            .trim_end_matches('/'),
        new_name,
    );
    Ok(new_rel)
}

/// 递归复制目录 / 文件（不含符号链接）
fn copy_recursive(src: &Path, dst: &Path) -> AppResult<u64> {
    copy_recursive_inner(src, dst, 0)
}

fn copy_recursive_inner(src: &Path, dst: &Path, depth: usize) -> AppResult<u64> {
    const MAX_COPY_DEPTH: usize = 32;
    if depth > MAX_COPY_DEPTH {
        return Err(AppError::Other(format!(
            "复制深度超限（{} 层），可能存在 junction 循环: {}",
            MAX_COPY_DEPTH,
            src.display()
        )));
    }
    let meta = std::fs::symlink_metadata(src)?;
    if meta.is_dir() {
        std::fs::create_dir_all(dst)?;
        let mut total = 0u64;
        for entry in std::fs::read_dir(src)?.flatten() {
            let ft = entry.file_type()?;
            if ft.is_symlink() {
                continue;
            }
            // Windows junction（reparse point 但非 symlink）深度递归防护：
            // file_type().is_symlink() 对 junction 返回 false，靠 depth 上限兜底
            total += copy_recursive_inner(&entry.path(), &dst.join(entry.file_name()), depth + 1)?;
        }
        Ok(total)
    } else {
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::copy(src, dst)
            .map_err(|e| AppError::Other(format!("复制 {} 失败: {}", src.display(), e)))
    }
}

/// 检查 dest 是否为 src 自身或其子目录（防止把目录粘贴进自身）
fn is_descendant(src_rel: &str, dest_rel: &str) -> bool {
    let s = src_rel.trim_matches('/');
    let d = dest_rel.trim_matches('/');
    if s.is_empty() {
        return true; // 项目根是所有路径祖先
    }
    d == s || d.starts_with(&format!("{}/", s))
}

/// 把源条目复制到目标目录（目标目录为空串表示项目根），返回新路径。
/// 同名冲突自动生成 "name (2)" 序号。
pub fn copy_entry(project_id: &str, src_path: &str, dest_dir: &str) -> AppResult<String> {
    let project = db::get_project(project_id)?;
    let root = Path::new(&project.root_path);
    if is_descendant(src_path, dest_dir) {
        return Err(AppError::Other("不能把条目复制到其自身内部".into()));
    }
    let src = safe_join(root, src_path)?;
    if !src.exists() {
        return Err(AppError::NotFound(format!("源不存在: {}", src_path)));
    }
    let dest_root_full = safe_join(root, dest_dir)?;
    if !dest_root_full.is_dir() {
        return Err(AppError::Other("目标不是目录".into()));
    }
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| AppError::Other("无效的源路径".into()))?;
    let final_name = unique_name(&dest_root_full, &name);
    let target = dest_root_full.join(&final_name);
    copy_recursive(&src, &target)?;
    Ok(join_rel(dest_dir, &final_name))
}

/// 把源条目移动到目标目录（同盘 rename，跨场景回退复制+删除），返回新路径。
pub fn move_entry(project_id: &str, src_path: &str, dest_dir: &str) -> AppResult<String> {
    let project = db::get_project(project_id)?;
    let root = Path::new(&project.root_path);
    if is_descendant(src_path, dest_dir) {
        return Err(AppError::Other("不能把条目移动到其自身内部".into()));
    }
    let src = safe_join(root, src_path)?;
    if !src.exists() {
        return Err(AppError::NotFound(format!("源不存在: {}", src_path)));
    }
    let parent_rel = Path::new(src_path)
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    let same_parent = normalize_rel(&parent_rel) == normalize_rel(dest_dir);
    if same_parent {
        return Err(AppError::Other("源与目标在同一目录".into()));
    }
    let dest_root_full = safe_join(root, dest_dir)?;
    if !dest_root_full.is_dir() {
        return Err(AppError::Other("目标不是目录".into()));
    }
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| AppError::Other("无效的源路径".into()))?;
    let final_name = unique_name(&dest_root_full, &name);
    let target = dest_root_full.join(&final_name);
    if let Err(rename_err) = std::fs::rename(&src, &target) {
        // 同卷 rename 失败（极少见）：回退为递归复制 + 删除源
        copy_recursive(&src, &target)?;
        std::fs::remove_dir_all(&src)
            .or_else(|_| std::fs::remove_file(&src))
            .map_err(|_| AppError::Other(format!("移动失败: {}", rename_err)))?;
    }
    Ok(join_rel(dest_dir, &final_name))
}

/// 归一化相对路径用于比较：统一斜杠、去掉首尾分隔符、压缩空段
fn normalize_rel(rel: &str) -> String {
    rel.replace('\\', "/")
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

/// 在系统文件管理器中显示条目：
/// - 文件 / 目录均用 explorer /select 定位并高亮
pub fn reveal_in_file_manager(project_id: &str, path: &str) -> AppResult<()> {
    let project = db::get_project(project_id)?;
    let full = safe_join(Path::new(&project.root_path), path)?;
    if !full.exists() {
        return Err(AppError::NotFound(format!("文件不存在: {}", path)));
    }
    // repo root 来自 `git rev-parse --show-toplevel`（Git for Windows 输出正斜杠），
    // 拼出的路径可能是混合分隔符；explorer /select 无法在含 `/` 的路径中定位选中项，
    // 会打开资源管理器但不跳转——统一 canonicalize 成规范反斜杠绝对路径。
    #[cfg(windows)]
    let target = crate::util::canonicalize_clean(&full).unwrap_or(full);
    #[cfg(not(windows))]
    let target = full;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", target.display()))
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|e| AppError::Other(format!("打开文件管理器失败: {}", e)))?;
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("xdg-open")
            .arg(target.parent().unwrap_or(&target))
            .spawn()
            .map_err(|e| AppError::Other(format!("打开文件管理器失败: {}", e)))?;
    }
    Ok(())
}

/// 在系统文件管理器中定位显示指定绝对路径（不依赖项目根目录）
pub fn reveal_path(path: &str) -> AppResult<()> {
    let full = PathBuf::from(path);
    if !full.exists() {
        return Err(AppError::NotFound(format!("文件不存在: {}", path)));
    }
    #[cfg(windows)]
    let target = crate::util::canonicalize_clean(&full).unwrap_or(full);
    #[cfg(not(windows))]
    let target = full;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", target.display()))
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|e| AppError::Other(format!("打开文件管理器失败: {}", e)))?;
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("xdg-open")
            .arg(target.parent().unwrap_or(&target))
            .spawn()
            .map_err(|e| AppError::Other(format!("打开文件管理器失败: {}", e)))?;
    }
    Ok(())
}