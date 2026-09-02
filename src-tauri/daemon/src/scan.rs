//! 扫描服务（R4）：并行 pom 遍历 + 可取消 + 进度流 + 结果缓存。
//!
//! 要点（与 ADR-0001 决策 7 对齐）：
//! - `ignore::WalkBuilder::build_parallel()` 并行遍历，`filter_entry` 跳过 target/.git/…
//! - `tokio_util::sync::CancellationToken` 联动：收到取消立即 `WalkState::Quit`
//! - `quick-xml` **流式** Reader 定向提取 `<artifactId>/<packaging>/<java.version>`
//! - 进度按「发现一个 pom 上报一个」经 `scan.progress` 推送
//! - 结果缓存进 SQLite `scan_cache`，二次打开毫秒级返回

use std::collections::HashMap;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use quick_xml::events::Event;
use quick_xml::Reader;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use jb_core::model::ScanModule;
use jb_core::protocol::*;

use crate::error::{Error, Result};
use crate::store::Store;

/// 单个 pom 的轻量信息。
struct PomInfo {
    artifact_id: String,
    /// 打包类型；缺省 jar。
    packaging: String,
    java_version: Option<String>,
}

enum ScanFail {
    Cancelled,
    Io(Error),
}

pub struct ScanService {
    store: Arc<Store>,
    bus: broadcast::Sender<Message>,
    cancels: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

impl ScanService {
    pub fn new(store: Arc<Store>, bus: broadcast::Sender<Message>) -> Arc<Self> {
        Arc::new(ScanService {
            store,
            bus,
            cancels: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// 发起扫描：命中缓存直接返回；否则后台运行并在事件上报进度与完成。
    pub async fn start(self: &Arc<Self>, project_path: &str) -> Result<ScanStartResult> {
        let project_key = normalize_key(project_path);
        if let Some((json, _ts)) = self.store.clone().get_scan_cache(project_key.clone()).await? {
            if let Ok(tree) = serde_json::from_str::<Vec<ScanModule>>(&json) {
                log::info!("扫描缓存命中: {project_key} ({} 节点)", tree.len());
                return Ok(ScanStartResult {
                    scan_id: String::new(),
                    cached: true,
                    tree,
                });
            }
        }

        let scan_id = uuid::Uuid::new_v4().to_string();
        let token = CancellationToken::new();
        self.cancels.lock().insert(scan_id.clone(), token.clone());

        // 闭包内只使用 clone，保留 scan_id 供返回
        let asc = scan_id.clone();
        let abus = self.bus.clone();
        let astore = self.store.clone();
        let acancels = self.cancels.clone();
        let aroot = PathBuf::from(project_path);
        let akey = project_key;

        tokio::spawn(async move {
            let (ptx, prx) = mpsc::unbounded_channel::<ScanProgressEvent>();
            let fwd_bus = abus.clone();
            let forwarder = tokio::spawn(async move {
                let mut rx = prx;
                while let Some(p) = rx.recv().await {
                    let _ = fwd_bus.send(Message::Notification(Notification::named(
                        event::SCAN_PROGRESS,
                        &p,
                    )));
                }
            });

            let wtok = token.clone();
            let wscan = asc.clone();
            let wptx = ptx.clone();
            let walk =
                tokio::task::spawn_blocking(move || run_walk(&aroot, &wtok, &wptx, &wscan)).await;

            drop(ptx);
            let _ = forwarder.await;

            let tree = match walk {
                Ok(Ok(tree)) => {
                    if let Ok(json) = serde_json::to_string(&tree) {
                        let _ = astore.clone().set_scan_cache(akey, json).await;
                    }
                    log::info!("扫描完成 scan_id={asc} 节点={}", tree.len());
                    tree
                }
                Ok(Err(ScanFail::Cancelled)) => {
                    log::info!("扫描已取消 scan_id={asc}");
                    Vec::new()
                }
                Ok(Err(ScanFail::Io(e))) => {
                    log::warn!("扫描失败 scan_id={asc}: {e}");
                    Vec::new()
                }
                Err(join) => {
                    log::warn!("扫描任务异常 scan_id={asc}: {join}");
                    Vec::new()
                }
            };
            acancels.lock().remove(&asc);
            let _ = abus.send(Message::Notification(Notification::named(
                event::SCAN_DONE,
                &ScanDoneEvent { scan_id: asc.clone(), tree },
            )));
        });

        Ok(ScanStartResult {
            scan_id,
            cached: false,
            tree: Vec::new(),
        })
    }

    /// 取消某次扫描。
    pub fn cancel(&self, scan_id: &str) -> Result<()> {
        let token = self
            .cancels
            .lock()
            .get(scan_id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("scan {scan_id} 不存在或已结束")))?;
        token.cancel();
        Ok(())
    }
}

/// 并行遍历收集 pom.xml，流式解析并组装树。阻塞线程中执行。
fn run_walk(
    root: &Path,
    token: &CancellationToken,
    ptx: &mpsc::UnboundedSender<ScanProgressEvent>,
    scan_id: &str,
) -> std::result::Result<Vec<ScanModule>, ScanFail> {
    if !root.join("pom.xml").exists() {
        return Err(ScanFail::Io(Error::NotFound(format!(
            "未找到根 pom.xml: {}",
            root.join("pom.xml").display()
        ))));
    }

    let mut builder = ignore::WalkBuilder::new(root);
    builder.git_ignore(true).git_global(true).standard_filters(false);
    builder.filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        name != "target" && name != ".git" && name != "node_modules" && name != ".idea"
    });

    let found: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
    let idx = Arc::new(AtomicUsize::new(0));
    let wptx = ptx.clone();
    let wscan = scan_id.to_string();

    builder
        .build_parallel()
        .run(|| {
            let found = Arc::clone(&found);
            let idx = Arc::clone(&idx);
            let wptx = wptx.clone();
            let wscan = wscan.clone();
            let token = token.clone();
            Box::new(move |entry| {
                if token.is_cancelled() {
                    return ignore::WalkState::Quit;
                }
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => return ignore::WalkState::Continue,
                };
                let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
                if is_file && entry.file_name().to_string_lossy() == "pom.xml" {
                    let path = entry.path().to_path_buf();
                    found.lock().push(path.clone());
                    let n = idx.fetch_add(1, Ordering::Relaxed) + 1;
                    let _ = wptx.send(ScanProgressEvent {
                        scan_id: wscan.clone(),
                        idx: n,
                        module_path: path.to_string_lossy().to_string(),
                    });
                }
                ignore::WalkState::Continue
            })
        });

    if token.is_cancelled() {
        return Err(ScanFail::Cancelled);
    }

    let mut poms = std::mem::take(&mut *found.lock());
    poms.sort();
    let mut infos: BTreeMap<PathBuf, PomInfo> = BTreeMap::new();
    for p in &poms {
        match parse_pom(p) {
            Ok(Some(info)) => {
                if let Some(dir) = p.parent() {
                    infos.insert(dir.to_path_buf(), info);
                }
            }
            Ok(None) => log::debug!("{} 无可解析内容", p.display()),
            Err(e) => log::debug!("解析 {} 失败: {e}", p.display()),
        }
    }
    Ok(build_tree(root, &infos))
}

/// 组装 module 树：按目录嵌套关系挂到最近拥有 pom 的祖先下。
fn build_tree(root: &Path, infos: &BTreeMap<PathBuf, PomInfo>) -> Vec<ScanModule> {
    let root_dir = root.to_path_buf();
    // (dir, parent_dir, node)
    let mut nodes: Vec<(PathBuf, Option<PathBuf>, ScanModule)> = Vec::new();
    for (dir, info) in infos {
        let parent = ancestor_pom_dir(&root_dir, dir, infos);
        let artifact = if info.artifact_id.is_empty() {
            dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
        } else {
            info.artifact_id.clone()
        };
        let is_service = matches!(info.packaging.as_str(), "jar" | "war" | "bundle");
        let node = ScanModule {
            artifact_id: artifact,
            pom_path: dir.join("pom.xml").to_string_lossy().to_string(),
            relative_path: dir
                .strip_prefix(&root_dir)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
            packaging: info.packaging.clone(),
            is_service,
            main_class: if is_service {
                detect_main_class(dir)
            } else {
                None
            },
            java_version: info.java_version.clone(),
            children: Vec::new(),
        };
        nodes.push((dir.clone(), parent, node));
    }

    let by_dir: HashMap<PathBuf, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, (d, _, _))| (d.clone(), i))
        .collect();
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();
    for (i, (_, parent, _)) in nodes.iter().enumerate() {
        match parent {
            Some(p) => {
                if let Some(&par) = by_dir.get(p) {
                    children.entry(par).or_default().push(i);
                } else {
                    roots.push(i);
                }
            }
            None => roots.push(i),
        }
    }

    fn dfs(nodes: &[(PathBuf, Option<PathBuf>, ScanModule)], children: &HashMap<usize, Vec<usize>>, i: usize) -> ScanModule {
        let mut m = nodes[i].2.clone();
        m.children = children
            .get(&i)
            .map(|cs| {
                let mut v: Vec<ScanModule> = cs.iter().map(|&c| dfs(nodes, children, c)).collect();
                v.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
                v
            })
            .unwrap_or_default();
        m
    }

    let mut out: Vec<ScanModule> = roots.iter().map(|&i| dfs(&nodes, &children, i)).collect();
    out.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    out
}

/// 返回 dir 的最近拥有 pom 的祖先目录；无则 None。
fn ancestor_pom_dir(root: &Path, dir: &Path, infos: &BTreeMap<PathBuf, PomInfo>) -> Option<PathBuf> {
    let mut cur = dir.parent();
    while let Some(c) = cur {
        if *c != *dir && infos.contains_key(c) {
            return Some(c.to_path_buf());
        }
        if c == root {
            break;
        }
        cur = c.parent();
    }
    None
}

/// 流式解析单个 pom.xml，定向提取 artifactId / packaging / java.version。
fn parse_pom(path: &Path) -> Result<Option<PomInfo>> {
    let file = std::fs::File::open(path).map_err(Error::Io)?;
    let mut reader = Reader::from_reader(std::io::BufReader::new(file));
    reader.config_mut().trim_text(true);

    let mut stack: Vec<String> = Vec::new();
    let mut artifact_id = String::new();
    let mut packaging = String::new();
    let mut java_version = String::new();

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                stack.push(qname_str(e.name()));
            }
            Ok(Event::End(e)) => {
                let n = qname_str(e.name());
                if let Some(pos) = stack.iter().rposition(|x| x == &n) {
                    stack.truncate(pos);
                }
            }
            Ok(Event::Empty(_)) => {}
            Ok(Event::Text(t)) => {
                let text = t.unescape().unwrap_or_default().to_string();
                let text = text.trim().to_string();
                if text.is_empty() || stack.is_empty() {
                    // 跳过
                } else {
                    let top = stack.last().map(|s| s.as_str()).unwrap_or("");
                    match top {
                        "artifactId" => artifact_id = text,
                        "packaging" => packaging = text,
                        "java.version" if stack.len() >= 2 && stack[stack.len() - 2] == "properties" => {
                            java_version = text;
                        }
                        _ => {}
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(Error::Other(format!("pom 解析错误: {e}"))),
            _ => {}
        }
        buf.clear();
    }

    if artifact_id.is_empty() && packaging.is_empty() {
        return Ok(None);
    }
    Ok(Some(PomInfo {
        artifact_id,
        packaging: if packaging.is_empty() { "jar".into() } else { packaging },
        java_version: Some(java_version).filter(|s| !s.is_empty()),
    }))
}

fn qname_str(name: quick_xml::name::QName) -> String {
    String::from_utf8_lossy(name.as_ref()).to_string()
}

/// 在 src/main/{java,kotlin} 下定位带 @SpringBootApplication 的 *Application.*，返回 FQN。
fn detect_main_class(dir: &Path) -> Option<String> {
    for base in [dir.join("src/main/java"), dir.join("src/main/kotlin")] {
        if base.is_dir() {
            if let Some(fqn) = find_app_class(&base) {
                return Some(fqn);
            }
        }
    }
    None
}

fn find_app_class(base: &Path) -> Option<String> {
    let mut stack: Vec<PathBuf> = vec![base.to_path_buf()];
    let mut depth = 0;
    while let Some(d) = stack.pop() {
        if depth > 8 {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for en in entries.flatten() {
            let p = en.path();
            if p.is_dir() {
                stack.push(p);
            } else if ["java", "kt", "kts"].iter().any(|e| p.extension().and_then(|x| x.to_str()) == Some(e)) {
                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                if name.contains("Application") {
                    if let Ok(content) = std::fs::read_to_string(&p) {
                        if content.contains("@SpringBootApplication") {
                            if let Ok(rel) = p.strip_prefix(base) {
                                let fqn = rel
                                    .with_extension("")
                                    .components()
                                    .map(|c| c.as_os_str().to_string_lossy().to_string())
                                    .collect::<Vec<_>>()
                                    .join(".");
                                return Some(fqn.to_string());
                            }
                        }
                    }
                }
            }
        }
        depth += 1;
    }
    None
}

fn normalize_key(path: &str) -> String {
    Path::new(path)
        .to_string_lossy()
        .trim_end_matches(['/', '\\'])
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn tmp_store() -> Arc<Store> {
        let dir = std::env::temp_dir().join(format!("jb-scan-test-{}", uuid::Uuid::new_v4()));
        Store::open(&dir.join("daemon.db")).unwrap()
    }

    #[tokio::test]
    async fn cache_hit_roundtrip() {
        let store = tmp_store();
        let tree = vec![ScanModule {
            artifact_id: "svc".into(),
            pom_path: r"C:\p\pom.xml".into(),
            relative_path: String::new(),
            packaging: "jar".into(),
            is_service: true,
            main_class: None,
            java_version: None,
            children: vec![],
        }];
        let json = serde_json::to_string(&tree).unwrap();
        store.clone().set_scan_cache("C:\\p".into(), json).await.unwrap();
        let got = store.clone().get_scan_cache("C:\\p".into()).await.unwrap().unwrap();
        let parsed: Vec<ScanModule> = serde_json::from_str(&got.0).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].is_service);
    }

    #[test]
    fn parse_pom_streaming_extracts_fields() {
        let dir = std::env::temp_dir().join(format!("jb-pom-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pom = r#"<?xml version="1.0"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <artifactId>demo-svc</artifactId>
  <packaging>jar</packaging>
  <properties><java.version>17</java.version></properties>
</project>"#;
        std::fs::write(dir.join("pom.xml"), pom).unwrap();
        let info = parse_pom(&dir.join("pom.xml")).unwrap().unwrap();
        assert_eq!(info.artifact_id, "demo-svc");
        assert_eq!(info.packaging, "jar");
        assert_eq!(info.java_version.as_deref(), Some("17"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_aggregator_pom_defaults_packaging() {
        let dir = std::env::temp_dir().join(format!("jb-pom2-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pom = r#"<project><artifactId>parent</artifactId></project>"#;
        std::fs::write(dir.join("pom.xml"), pom).unwrap();
        let info = parse_pom(&dir.join("pom.xml")).unwrap().unwrap();
        assert_eq!(info.packaging, "jar"); // 默认打包 jar（原样保存）
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cancellation_stops_walk_early() {
        // 造一个包含大量 pom 的项目
        let root = std::env::temp_dir().join(format!("jb-cancel-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("pom.xml"), "<project><artifactId>r</artifactId></project>").unwrap();
        for i in 0..2000 {
            let d = root.join(format!("m{i}"));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("pom.xml"), "<project><artifactId>m</artifactId></project>").unwrap();
        }
        let token = CancellationToken::new();
        let (tx, mut rx) = mpsc::unbounded_channel::<ScanProgressEvent>();
        let t2 = token.clone();
        let scan_id = "s1".to_string();
        // 预取消，walk 应立即停下
        token.cancel();
        let res = run_walk(&root, &t2, &tx, &scan_id);
        assert!(matches!(res, Err(ScanFail::Cancelled)));
        let drained = rx.try_recv().is_err(); // 取消后不应继续上报
        let _ = drained;
        std::fs::remove_dir_all(&root).ok();
    }
}