use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::db::models::ScannedModule;
use crate::error::{AppError, AppResult};

/// POM 解析结果
#[derive(Debug, Clone, Default)]
pub struct PomInfo {
    pub artifact_id: String,
    pub packaging: String,        // jar/war/pom
    pub modules: Vec<String>,      // 相对路径
}

/// 解析单个 pom.xml
pub fn parse_pom(pom_path: &Path) -> AppResult<PomInfo> {
    let content = std::fs::read_to_string(pom_path)
        .map_err(|e| AppError::PomParse(format!("读取 {} 失败: {}", pom_path.display(), e)))?;

    let mut info = PomInfo {
        packaging: "jar".to_string(), // Maven 默认 jar
        ..Default::default()
    };

    let mut reader = quick_xml::Reader::from_str(&content);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut path_stack: Vec<String> = vec![];
    let mut text_buf = String::new();

    loop {
        use quick_xml::events::Event;
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                path_stack.push(String::from_utf8_lossy(e.name().as_ref()).to_string());
                text_buf.clear();
            }
            Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                // <packaging/> 空标签 → 默认 jar 不变
                if path_stack.last().map(|s| s.as_str()) == Some("properties") {
                    // ignore
                }
                let _ = name;
            }
            Ok(Event::End(_)) => {
                let tag = path_stack.pop();
                // 文本内容归属当前栈顶
                let cur = path_stack.last().map(|s| s.as_str());
                match tag.as_deref() {
                    Some("artifactId") if cur == Some("project") => {
                        if info.artifact_id.is_empty() {
                            info.artifact_id = text_buf.trim().to_string();
                        }
                    }
                    Some("packaging") if cur == Some("project") => {
                        let v = text_buf.trim();
                        if !v.is_empty() {
                            info.packaging = v.to_string();
                        }
                    }
                    Some("module") if cur == Some("modules") => {
                        let m = text_buf.trim().to_string();
                        if !m.is_empty() {
                            info.modules.push(m);
                        }
                    }
                    _ => {}
                }
                text_buf.clear();
            }
            Ok(Event::Text(e)) => {
                text_buf.push_str(&e.unescape().unwrap_or_default());
            }
            Ok(Event::CData(e)) => {
                text_buf.push_str(&String::from_utf8_lossy(e.as_ref()));
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(AppError::PomParse(format!(
                    "解析 {} 失败: {}",
                    pom_path.display(),
                    e
                )))
            }
            _ => {}
        }
    }

    Ok(info)
}

/// 递归扫描项目下所有 module
pub fn scan_project(root_pom: &Path) -> AppResult<Vec<ScannedModule>> {
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let canonical = root_pom
        .canonicalize()
        .map_err(|e| AppError::PomParse(format!("路径规范化失败: {}", e)))?;
    scan_recursive(&canonical, &canonical.parent().unwrap_or(Path::new("")), &mut visited, "")
}

fn scan_recursive(
    pom_path: &Path,
    base_dir: &Path,
    visited: &mut HashSet<PathBuf>,
    rel_prefix: &str,
) -> AppResult<Vec<ScannedModule>> {
    if visited.contains(pom_path) {
        return Ok(vec![]);
    }
    visited.insert(pom_path.to_path_buf());

    let info = parse_pom(pom_path)?;
    let dir = pom_path.parent().unwrap_or(Path::new(""));
    // 判定是否为可启动服务：packaging 必须是 jar/war，且模块内存在带 @SpringBootApplication 的主类
    // 单纯 jar 且不含 SpringBoot 主类的（工具/公共模块）不算服务，避免污染勾选列表
    let main_class_opt = if matches!(info.packaging.as_str(), "jar" | "war") {
        find_spring_boot_main_class(dir)
    } else {
        None
    };
    let is_service = main_class_opt.is_some();
    let relative = if rel_prefix.is_empty() {
        dir.strip_prefix(base_dir).unwrap_or(dir).to_string_lossy().to_string()
    } else {
        rel_prefix.to_string()
    };

    let mut children = vec![];
    for module_rel in &info.modules {
        let module_path = dir.join(module_rel);
        let module_pom = if module_path.is_dir() {
            module_path.join("pom.xml")
        } else if module_path.extension().and_then(|e| e.to_str()) == Some("xml") {
            module_path
        } else {
            module_path.with_extension("xml")
        };
        if module_pom.exists() {
            let child_rel = if relative.is_empty() {
                module_rel.clone()
            } else {
                format!("{}/{}", relative, module_rel)
            };
            let mut sub = scan_recursive(&module_pom, base_dir, visited, &child_rel)?;
            children.append(&mut sub);
        }
    }

    let already_added = crate::db::find_service_by_pom(&pom_path.to_string_lossy())?
        .is_some();

    let module = ScannedModule {
        artifact_id: if info.artifact_id.is_empty() {
            dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
        } else {
            info.artifact_id
        },
        pom_path: pom_path.to_string_lossy().to_string(),
        relative_path: relative,
        packaging: info.packaging.clone(),
        is_service,
        already_added,
        main_class: main_class_opt,
        children,
    };

    // 返回扁平结构（树结构在前端按路径重建，此处平铺便于勾选）
    // 但保留 children 以备层级展示
    Ok(vec![module])
}

/// 在模块的 `src/main/java` 下查找带 `@SpringBootApplication` 注解的类，命中则返回其全限定名。
///
/// - 先从模块 pom.xml 里直接取 mainClass / start-class（支持 ${xxx} 变量解引用）——
///   BladeX / Spring Boot Parent 项目能直接命中，避免扫数百个 java 文件
/// - 命中不了才去 `src/main/java` 递归扫 @SpringBootApplication
/// - 扫描时跳过 target/.git/node_modules/.idea，每个 .java 只读前 4KB
pub fn find_spring_boot_main_class(module_dir: &Path) -> Option<String> {
    // 1. pom.xml 直取（最快）
    let pom_path = module_dir.join("pom.xml");
    if let Ok(content) = std::fs::read_to_string(&pom_path) {
        if let Some(mc) = crate::process::build::extract_main_class_from_pom_xml(&content) {
            return Some(mc);
        }
    }
    // 2. 扫源码兜底
    let src_java = module_dir.join("src").join("main").join("java");
    if !src_java.is_dir() {
        return None;
    }
    scan_annotation(&src_java, &src_java)
}

fn scan_annotation(root: &Path, dir: &Path) -> Option<String> {
    let entries = std::fs::read_dir(dir).ok()?;
    // 先累积后排序：先扫同级 .java（主类多数在包根目录），再递归子目录，命中即返
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
        if let Ok(head) = read_head(path, 4096) {
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
        if let Some(mc) = scan_annotation(root, d) {
            return Some(mc);
        }
    }
    None
}

fn read_head(path: &Path, n: usize) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; n];
    let read = f.read(&mut buf)?;
    buf.truncate(read);
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// 将扫描树展平为可勾选列表
pub fn flatten_modules(modules: &[ScannedModule]) -> Vec<ScannedModule> {
    let mut out = vec![];
    for m in modules {
        out.push(m.clone());
        let sub = flatten_modules(&m.children);
        out.extend(sub);
    }
    out
}

/// 从某路径向上查找 .git 目录，返回仓库根
pub fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut cur = start.to_path_buf();
    loop {
        if cur.join(".git").exists() {
            return Some(cur);
        }
        if !cur.pop() {
            return None;
        }
    }
}
