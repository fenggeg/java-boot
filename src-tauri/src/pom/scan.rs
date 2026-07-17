//! 源码扫描工具：在 `src/main/java` 下查找带 `@SpringBootApplication` 注解的类。
//!
//! 从 pom/mod.rs 和 process/build.rs 中提取共享，避免代码重复。
//! 扫描时跳过 target/.git/node_modules/.idea，每个 .java 只读前 16KB。

use std::io::Read;
use std::path::{Path, PathBuf};

/// 在模块的 `src/main/java` 下查找带 `@SpringBootApplication` 注解的类，返回全限定名。
///
/// 完整流程：
/// - 先从 pom.xml 里直接取 mainClass / start-class（支持 ${xxx} 变量解引用）
/// - 命中不了才去 `src/main/java` 递归扫 @SpringBootApplication
///
/// 适用于后台预热等"从零开始探测"的场景。若调用方已自行解析过 pom.xml，
/// 应直接调 [`scan_source_for_main_class`] 避免重复读取。
pub fn find_spring_boot_main_class(module_dir: &Path) -> Option<String> {
    // 1. pom.xml 直取（最快）
    let pom_path = module_dir.join("pom.xml");
    if let Ok(content) = std::fs::read_to_string(&pom_path) {
        if let Some(mc) = crate::process::build::extract_main_class_from_pom_xml(&content) {
            return Some(mc);
        }
    }
    // 2. 扫源码兜底
    scan_source_for_main_class(module_dir)
}

/// 仅扫源码找 @SpringBootApplication 主类（不读 pom.xml）。
///
/// 供已自行解析过 pom.xml 的调用方（如 `build::detect_main_class`）使用，
/// 避免重复读取 pom.xml。
pub fn scan_source_for_main_class(module_dir: &Path) -> Option<String> {
    let src_java = module_dir.join("src").join("main").join("java");
    if !src_java.is_dir() {
        return None;
    }
    scan_spring_application(&src_java, &src_java)
}

/// 递归扫描 java 文件找 `@SpringBootApplication`（只读文件头 16KB，覆盖大量 import/license header）
///
/// **同级优先**：先扫当前目录下的 .java，再递归子目录。
fn scan_spring_application(root: &Path, dir: &Path) -> Option<String> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut files: Vec<PathBuf> = vec![];
    let mut dirs: Vec<PathBuf> = vec![];
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if matches!(name, "target" | ".git" | "node_modules" | ".idea") {
                continue;
            }
            dirs.push(path);
        } else if path.extension().and_then(|e| e.to_str()) == Some("java") {
            files.push(path);
        }
    }
    for path in &files {
        if let Ok(head) = read_head(path, 16384) {
            if head.contains("@SpringBootApplication") {
                if let Ok(rel) = path.strip_prefix(root) {
                    let fqcn = rel
                        .to_string_lossy()
                        .replace('\\', "/")
                        .replace('/', ".")
                        .trim_end_matches(".java")
                        .to_string();
                    return Some(fqcn);
                }
            }
        }
    }
    for d in &dirs {
        if let Some(mc) = scan_spring_application(root, d) {
            return Some(mc);
        }
    }
    None
}

/// 读取文件前 N 字节（不足则读整份）；用于扫注解，比 read_to_string 快得多
pub fn read_head(path: &Path, n: usize) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; n];
    let read = f.read(&mut buf)?;
    buf.truncate(read);
    Ok(String::from_utf8_lossy(&buf).into_owned())
}