//! 构建/编译支持模块
//!
//! 拆自原 manager.rs，包含 SpringBoot 启动链路的构建优化：
//! - `run_mvn_capture`：统一的 Maven 阻塞式执行器，实时推 [mvn] 日志
//! - `detect_main_class`：主类探测（服务表 → pom → 源码扫描 · 首 4KB），命中后回写 DB
//! - `is_module_up_to_date`：基于 mtime 的模块新旧判定
//! - `ClasspathCache`：把 `target/.javaboot-cp.txt` 变成真缓存（原实现读完就 remove）
//! - `decide_build_strategy`：给 start() 用的三档决策（Skip / Compile / CompileAll）

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use parking_lot::Mutex as PMutex;
use tauri::AppHandle;

use crate::db;
use crate::db::models::Service;
use crate::error::{AppError, AppResult};

use super::env::{inject_env, EnvConfig};
use super::log_pipe::emit_log_raw;

/// 编译期子进程 PID，用于 stop 时中断
pub type CompilePidSlot = Arc<PMutex<Option<u32>>>;

// ================================================================
// Maven 通用执行器
// ================================================================

/// 阻塞式跑 mvn，实时把 stdout/stderr 用 [mvn] 前缀推给前端
///
/// 相较原实现的改进：
/// - 抽出单一入口，`prepare_dependencies` / `build_classpath` / `compile_and_start`
///   共用，不再各自重复 spawn+BufReader 代码
/// - stdout 与 stderr **分别** 起线程读取，避免原 `build_classpath` 里先 stdout
///   收完再 stderr、导致混合输出乱序 & 阻塞的问题
pub fn run_mvn_capture(
    program: &str,
    args: &[String],
    cwd: &Path,
    env_cfg: &EnvConfig,
    compile_pid: CompilePidSlot,
    app: AppHandle,
    service_id: String,
) -> std::io::Result<std::process::ExitStatus> {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    cmd.current_dir(cwd);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.stdin(std::process::Stdio::null());
    inject_env(&mut cmd, env_cfg);

    let mut child = cmd.spawn()?;
    *compile_pid.lock() = Some(child.id());

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // 起两个后台线程分别读取 stdout / stderr，主线程等 wait
    let app_out = app.clone();
    let sid_out = service_id.clone();
    let t_out = std::thread::spawn(move || {
        if let Some(out) = stdout {
            let reader = std::io::BufReader::new(out);
            for line in reader.lines().flatten() {
                emit_log_raw(&app_out, &sid_out, "[mvn]", &line);
            }
        }
    });
    let app_err = app.clone();
    let sid_err = service_id.clone();
    let t_err = std::thread::spawn(move || {
        if let Some(err) = stderr {
            let reader = std::io::BufReader::new(err);
            for line in reader.lines().flatten() {
                emit_log_raw(&app_err, &sid_err, "[mvn]", &line);
            }
        }
    });

    let status = child.wait()?;
    let _ = t_out.join();
    let _ = t_err.join();
    *compile_pid.lock() = None;
    Ok(status)
}

/// mvn 通用参数：并行 + 静默进度条 + 跳过 spring-boot repackage
///
/// - `-T 1C` 每个 CPU 一个线程编译（多模块下大幅提速）
/// - `--no-transfer-progress` 关下载进度条噪音
/// - `-Dspring-boot.repackage.skip=true` 跳过 spring-boot-maven-plugin 的 repackage
///   （原来 `mvn install` 会顺带跑 repackage，几十秒到几分钟纯浪费）
/// - `-Dmaven.test.skip=true` 完全跳过 test-compile 与 test 阶段
pub fn common_mvn_flags() -> Vec<String> {
    vec![
        "-T".into(),
        "1C".into(),
        "--no-transfer-progress".into(),
        "-Dspring-boot.repackage.skip=true".into(),
        "-Dmaven.test.skip=true".into(),
        "-DskipTests".into(),
    ]
}

// ================================================================
// 主类探测
// ================================================================

/// 探测主类：先查 DB 缓存 → pom.xml → 扫源码，命中后回写 service.main_class
pub fn detect_main_class(service: &Service, working_dir: &Path) -> AppResult<String> {
    // 0. DB 缓存命中
    if let Some(mc) = &service.main_class {
        let t = mc.trim();
        if !t.is_empty() {
            return Ok(t.to_string());
        }
    }
    // 1. pom.xml 里 spring-boot-maven-plugin.mainClass
    let pom_path = working_dir.join("pom.xml");
    if pom_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&pom_path) {
            if let Some(mc) = extract_main_class_from_pom_xml(&content) {
                let _ = db::set_service_main_class(&service.id, &mc);
                return Ok(mc);
            }
        }
    }
    // 2. 扫 src/main/java 找 @SpringBootApplication（只读文件头 4KB，够看注解和类名）
    let src_java = working_dir.join("src").join("main").join("java");
    if src_java.exists() {
        if let Some(mc) = scan_spring_application(&src_java, &src_java) {
            let _ = db::set_service_main_class(&service.id, &mc);
            return Ok(mc);
        }
    }
    Err(AppError::Process(format!(
        "未找到主类（mainClass）：{}\n请在服务配置或 pom.xml 的 spring-boot-maven-plugin 中指定 mainClass。",
        service.name
    )))
}

/// 从 pom.xml 文本中提取 `<mainClass>xxx</mainClass>`（跳过注释）
pub fn extract_main_class_from_pom_xml(content: &str) -> Option<String> {
    let mut in_comment = false;
    let mut i = 0;
    let bytes = content.as_bytes();
    while i < bytes.len() {
        if !in_comment && content[i..].starts_with("<!--") {
            in_comment = true;
            i += 4;
            continue;
        }
        if in_comment {
            if content[i..].starts_with("-->") {
                in_comment = false;
                i += 3;
                continue;
            }
            i += 1;
            continue;
        }
        if content[i..].starts_with("<mainClass>") {
            if let Some(end) = content[i + 11..].find("</mainClass>") {
                let mc = &content[i + 11..i + 11 + end];
                let trimmed = mc.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
            if let Some(pos) = content[i + 11..].find("</mainClass>") {
                i = i + 11 + pos + 12;
                continue;
            }
            break;
        }
        i += 1;
    }
    None
}

/// 递归扫描 java 文件找 `@SpringBootApplication`（只读文件头 4KB，命中概率 99%）
fn scan_spring_application(root: &Path, dir: &Path) -> Option<String> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(mc) = scan_spring_application(root, &path) {
                return Some(mc);
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("java") {
            if let Ok(head) = read_head(&path, 4096) {
                if head.contains("@SpringBootApplication") {
                    if let Ok(rel) = path.strip_prefix(root) {
                        let class_path = rel
                            .to_string_lossy()
                            .replace('\\', "/")
                            .replace('/', ".");
                        let fqcn = class_path.trim_end_matches(".java").to_string();
                        return Some(fqcn);
                    }
                }
            }
        }
    }
    None
}

/// 读取文件前 N 字节（不足则读整份）；用于扫注解，比 read_to_string 快得多
fn read_head(path: &Path, n: usize) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; n];
    let read = f.read(&mut buf)?;
    buf.truncate(read);
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

// ================================================================
// mtime 判定
// ================================================================

/// 递归取目录下最新的 mtime，跳过 target / .git / node_modules
pub fn max_mtime(dir: &Path) -> Option<SystemTime> {
    let mut latest: Option<SystemTime> = None;
    let _ = walk(dir, &mut |p| {
        if let Ok(md) = p.metadata() {
            if let Ok(t) = md.modified() {
                latest = Some(latest.map_or(t, |cur| cur.max(t)));
            }
        }
    });
    latest
}

fn walk<F: FnMut(&Path)>(dir: &Path, f: &mut F) -> std::io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            let name = entry.file_name();
            let n = name.to_string_lossy();
            if n == "target" || n == ".git" || n == "node_modules" {
                continue;
            }
            walk(&path, f)?;
        } else {
            f(&path);
        }
    }
    Ok(())
}

/// 模块的 src/main（含 resources）是否比 target/classes 新
///
/// 返回 true 表示 classes 已经是最新的、可跳过编译
pub fn is_module_up_to_date(module_dir: &Path) -> bool {
    let classes = module_dir.join("target").join("classes");
    if !classes.exists() {
        return false;
    }
    let src = module_dir.join("src").join("main");
    if !src.exists() {
        // 没有 src/main，认为已就绪（依赖模块可能只有 resources）
        return true;
    }
    let src_max = match max_mtime(&src) {
        Some(t) => t,
        None => return true,
    };
    let classes_max = match max_mtime(&classes) {
        Some(t) => t,
        None => return false,
    };
    classes_max >= src_max
}

/// pom.xml 是否比 classpath cache 新（决定是否要刷新 classpath）
#[allow(dead_code)]
pub fn pom_newer_than(pom_path: &Path, cache_path: &Path) -> bool {
    let pom_mt = std::fs::metadata(pom_path)
        .and_then(|m| m.modified())
        .ok();
    let cache_mt = std::fs::metadata(cache_path)
        .and_then(|m| m.modified())
        .ok();
    match (pom_mt, cache_mt) {
        (Some(a), Some(b)) => a > b,
        (Some(_), None) => true, // cache 还没有
        _ => false,
    }
}

// ================================================================
// Classpath 缓存
// ================================================================

/// classpath 缓存文件对
pub struct ClasspathCache {
    pub cp_file: PathBuf,
    pub key_file: PathBuf,
}

impl ClasspathCache {
    pub fn for_module(working_dir: &Path) -> Self {
        let target = working_dir.join("target");
        Self {
            cp_file: target.join(".javaboot-cp.txt"),
            key_file: target.join(".javaboot-cp.key"),
        }
    }

    /// 计算缓存 key：本模块 pom + 项目根 pom + maven_home 的哈希
    ///
    /// 只要 pom.xml 内容变了、maven 切换了，key 就变，从而强制重新解析 classpath。
    /// 不用文件 mtime 因为 pom 里改注释也会变 mtime 但依赖没变。
    pub fn compute_key(working_dir: &Path, env_cfg: &EnvConfig) -> String {
        let mut hasher = DefaultHasher::new();
        if let Ok(content) = std::fs::read_to_string(working_dir.join("pom.xml")) {
            content.hash(&mut hasher);
        }
        if let Some(root) = &env_cfg.project_root {
            if let Ok(content) = std::fs::read_to_string(Path::new(root).join("pom.xml")) {
                content.hash(&mut hasher);
            }
        }
        if let Some(mh) = &env_cfg.maven_home {
            mh.hash(&mut hasher);
        }
        format!("{:x}", hasher.finish())
    }

    /// 缓存是否有效
    pub fn is_valid(&self, key: &str) -> bool {
        if !self.cp_file.exists() || !self.key_file.exists() {
            return false;
        }
        match std::fs::read_to_string(&self.key_file) {
            Ok(s) => s.trim() == key,
            Err(_) => false,
        }
    }

    pub fn load(&self) -> Option<String> {
        std::fs::read_to_string(&self.cp_file)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn save(&self, cp: &str, key: &str) -> std::io::Result<()> {
        if let Some(dir) = self.cp_file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&self.cp_file, cp)?;
        std::fs::write(&self.key_file, key)?;
        Ok(())
    }
}

// ================================================================
// 构建策略决策
// ================================================================

/// 启动前的构建策略决策结果
#[derive(Debug, Clone, PartialEq)]
pub enum BuildStrategy {
    /// A：全部就绪，跳 mvn 直接 java（最理想）
    Skip,
    /// B：只当前模块源码变更，兄弟 classes 就绪 → mvn -pl <mod> compile
    CompileCurrent,
    /// C：兄弟模块缺 classes 或 pom 变更 → mvn -pl <mod> -am compile
    CompileAll,
}

/// 决策逻辑：
/// 1. classpath 缓存 key 不匹配 → 走 C（pom 变了、maven 换了）
/// 2. 当前模块 classes 不存在 → 走 C
/// 3. 当前模块 src 更新过 → 走 B
/// 4. 检查所有兄弟模块 target/classes 是否都存在 → 缺失 → C
/// 5. 全就绪 → A
pub fn decide_build_strategy(
    working_dir: &Path,
    env_cfg: &EnvConfig,
    cache_valid: bool,
) -> BuildStrategy {
    if !cache_valid {
        return BuildStrategy::CompileAll;
    }
    let classes = working_dir.join("target").join("classes");
    if !classes.exists() {
        return BuildStrategy::CompileAll;
    }
    if !is_module_up_to_date(working_dir) {
        // 当前模块源码变了：检查兄弟 classes 是否齐全，决定是否要 -am
        if let Some(root) = &env_cfg.project_root {
            if !siblings_all_built(Path::new(root), working_dir) {
                return BuildStrategy::CompileAll;
            }
        }
        return BuildStrategy::CompileCurrent;
    }
    // 当前模块 up to date，兜底再确认兄弟 classes
    if let Some(root) = &env_cfg.project_root {
        if !siblings_all_built(Path::new(root), working_dir) {
            return BuildStrategy::CompileAll;
        }
    }
    BuildStrategy::Skip
}

/// 兄弟模块的 target/classes 是否都存在（存在即当作 OK，不做 mtime 深检）
///
/// 只看根下一级和两级子目录，命中 pom.xml 的目录就当模块。
fn siblings_all_built(root: &Path, current: &Path) -> bool {
    let cur_canon = current.canonicalize().unwrap_or_else(|_| current.to_path_buf());
    let mut all_ok = true;
    let _ = std::fs::read_dir(root).map(|entries| {
        for entry in entries.flatten() {
            let p = entry.path();
            if !p.is_dir() {
                continue;
            }
            if p.canonicalize().unwrap_or_else(|_| p.clone()) == cur_canon {
                continue;
            }
            if p.join("pom.xml").exists() && !p.join("target").join("classes").exists() {
                all_ok = false;
                return;
            }
            // 嵌套一层
            if let Ok(sub) = std::fs::read_dir(&p) {
                for s in sub.flatten() {
                    let sp = s.path();
                    if !sp.is_dir() {
                        continue;
                    }
                    if sp.canonicalize().unwrap_or_else(|_| sp.clone()) == cur_canon {
                        continue;
                    }
                    if sp.join("pom.xml").exists()
                        && !sp.join("target").join("classes").exists()
                    {
                        all_ok = false;
                        return;
                    }
                }
            }
        }
    });
    all_ok
}

/// 收集兄弟模块的 target/classes 列表，加入 classpath
pub fn collect_sibling_classes(root: &Path, current: &Path) -> Vec<String> {
    let mut extra_cp: Vec<String> = vec![];
    let current_classes = current.join("target").join("classes");
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let p = entry.path();
            if !p.is_dir() {
                continue;
            }
            let tc = p.join("target").join("classes");
            if tc.exists() && tc != current_classes {
                extra_cp.push(tc.to_string_lossy().to_string());
            }
            if let Ok(sub) = std::fs::read_dir(&p) {
                for s in sub.flatten() {
                    let sp = s.path();
                    let tc = sp.join("target").join("classes");
                    if tc.exists() && tc != current_classes {
                        extra_cp.push(tc.to_string_lossy().to_string());
                    }
                }
            }
        }
    }
    extra_cp
}
