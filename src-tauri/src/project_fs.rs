//! 项目文件系统操作（文件浏览 / 预览 / 编辑）
//!
//! 基于 `project.root_path`，不依赖 Git 仓库（与 git.rs 的 read/write 区分）：
//! - `list_dir`：惰性列出单层目录，过滤构建产物/依赖等大目录
//! - `read_file`：UTF-8 优先；失败尝试 GBK 转码（中文项目常见），非 UTF-8 标记只读防写坏编码；超大文件只读预览
//! - `write_file`：仅写入 UTF-8 文本，路径校验防止越出项目根

use std::path::{Component, Path, PathBuf};

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