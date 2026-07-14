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
    let is_service = matches!(info.packaging.as_str(), "jar" | "war") || info.packaging == "jar";
    let dir = pom_path.parent().unwrap_or(Path::new(""));
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
        children,
    };

    // 返回扁平结构（树结构在前端按路径重建，此处平铺便于勾选）
    // 但保留 children 以备层级展示
    Ok(vec![module])
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
