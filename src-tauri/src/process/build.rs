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
use crate::util::NoWindow;

/// 编译期子进程 PID，用于 stop 时中断
pub type CompilePidSlot = Arc<PMutex<Option<u32>>>;

/// 剥掉 Windows 扩展长路径前缀 `\\?\` / `\\?\UNC\`。
///
/// `std::fs::canonicalize()` 在 Windows 上会返回 `\\?\D:\...` 形式的 verbatim 路径；
/// 老 Java / Plexus / 部分 mvn 插件不认识路径里的 `?`，会抛
/// `Illegal character [?] in path at index 2`。传给外部程序前需要剥掉。
pub fn strip_verbatim_prefix(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        // UNC 路径要还原为 \\server\share\...
        PathBuf::from(format!(r"\\{}", rest))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p.to_path_buf()
    }
}

// ================================================================
// Maven 通用执行器
// ================================================================

/// 单次 Maven 执行结果：退出码 + 全量输出文本（用于离线失败识别）
struct MvnRun {
    status: std::process::ExitStatus,
    /// stdout+stderr 按行累积的完整输出
    output: String,
}

/// 阻塞式跑一次 mvn，实时把 stdout/stderr 用 [mvn] 前缀推给前端，
/// 同时在内存累积输出供调用方做失败原因分析
///
/// 相较原实现的改进：
/// - 抽出单一入口，`prepare_dependencies` / `build_classpath` / `compile_and_start`
///   共用，不再各自重复 spawn+BufReader 代码
/// - stdout 与 stderr **分别** 起线程读取，避免原 `build_classpath` 里先 stdout
///   收完再 stderr、导致混合输出乱序 & 阻塞的问题
#[allow(clippy::too_many_arguments)]
fn run_mvn_once(
    program: &str,
    args: &[String],
    cwd: &Path,
    env_cfg: &EnvConfig,
    compile_pid: CompilePidSlot,
    app: AppHandle,
    service_id: String,
) -> std::io::Result<MvnRun> {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    cmd.current_dir(cwd);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.stdin(std::process::Stdio::null());
    inject_env(&mut cmd, env_cfg);
    cmd.creation_flags_no_window();

    let mut child = cmd.spawn()?;
    let child_pid = child.id();
    *compile_pid.lock() = Some(child_pid);

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let collected = Arc::new(PMutex::new(String::new()));

    // 起两个后台线程分别读取 stdout / stderr
    let app_out = app.clone();
    let sid_out = service_id.clone();
    let sink_out = collected.clone();
    let t_out = std::thread::spawn(move || {
        if let Some(out) = stdout {
            let reader = std::io::BufReader::new(out);
            for line in reader.lines().flatten() {
                sink_out.lock().push_str(&line);
                sink_out.lock().push('\n');
                emit_log_raw(&app_out, &sid_out, "[mvn]", &line);
            }
        }
    });
    let app_err = app.clone();
    let sid_err = service_id.clone();
    let sink_err = collected.clone();
    let t_err = std::thread::spawn(move || {
        if let Some(err) = stderr {
            let reader = std::io::BufReader::new(err);
            for line in reader.lines().flatten() {
                sink_err.lock().push_str(&line);
                sink_err.lock().push('\n');
                emit_log_raw(&app_err, &sid_err, "[mvn]", &line);
            }
        }
    });

    // 给 wait 加 10 分钟超时，防止 Maven 卡死导致 spawn_blocking 线程池耗尽
    let status = wait_with_timeout(&mut child, child_pid)?;
    let _ = t_out.join();
    let _ = t_err.join();
    *compile_pid.lock() = None;
    // 线程已 join，独占取回缓冲
    let output = Arc::try_unwrap(collected)
        .map(|m| m.into_inner())
        .unwrap_or_default();
    Ok(MvnRun { status, output })
}

/// 阻塞式跑 mvn（兼容入口）：只关心退出码
pub fn run_mvn_capture(
    program: &str,
    args: &[String],
    cwd: &Path,
    env_cfg: &EnvConfig,
    compile_pid: CompilePidSlot,
    app: AppHandle,
    service_id: String,
) -> std::io::Result<std::process::ExitStatus> {
    Ok(
        run_mvn_once(program, args, cwd, env_cfg, compile_pid, app, service_id)?
            .status,
    )
}

/// 离线失败的典型输出特征（小写匹配）：
/// 命中才值得回退在线重试；普通编译错误（javac 报错）不重试避免重复刷屏
const OFFLINE_FAILURE_MARKERS: &[&str] = &[
    "cannot access",
    "offline mode",
    "failure to find",
    "resolution will not be reattempted",
    "was cached in the local repository",
];

fn looks_like_offline_failure(output: &str) -> bool {
    let lower = output.to_lowercase();
    OFFLINE_FAILURE_MARKERS.iter().any(|m| lower.contains(m))
}

/// 离线优先执行：先带 `-o` 跳过远程仓库元数据检查（弱网/公司 Nexus 慢时显著提速）；
/// 仅当输出命中离线类错误特征时，自动去掉 `-o` 在线重试一次。
pub fn run_mvn_offline_first(
    program: &str,
    args: &[String],
    cwd: &Path,
    env_cfg: &EnvConfig,
    compile_pid: CompilePidSlot,
    app: AppHandle,
    service_id: String,
) -> std::io::Result<std::process::ExitStatus> {
    let mut offline_args: Vec<String> = Vec::with_capacity(args.len() + 1);
    offline_args.push("-o".into());
    offline_args.extend_from_slice(args);
    let first = run_mvn_once(
        program,
        &offline_args,
        cwd,
        env_cfg,
        compile_pid.clone(),
        app.clone(),
        service_id.clone(),
    )?;
    if first.status.success() {
        return Ok(first.status);
    }
    if looks_like_offline_failure(&first.output) {
        emit_log_raw(
            &app,
            &service_id,
            "[mvn]",
            "[javaboot] 离线模式失败（本地仓库缺构件/元数据），回退在线模式重试...",
        );
        let second =
            run_mvn_once(program, args, cwd, env_cfg, compile_pid, app, service_id)?;
        return Ok(second.status);
    }
    Ok(first.status)
}

/// 带超时的 child.wait()，超时则强杀进程防止线程泄漏
fn wait_with_timeout(child: &mut std::process::Child, pid: u32) -> std::io::Result<std::process::ExitStatus> {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    // 超时：强杀进程树，返回错误
                    let _ = kill_child(pid);
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("Maven 进程 {} 超时（600 秒），已强杀", pid),
                    ));
                }
                // 100ms 粒度：mvn 退出后尽快继续启动链路（500ms 平均多等 ~250ms）
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e),
        }
    }
}

/// 强杀进程树（委托 manager 的实现）
fn kill_child(pid: u32) {
    super::manager::kill_process_tree_by_pid(pid);
}

/// mvn 通用参数：并行 + 静默进度条 + 跳过 spring-boot repackage
///
/// - `-T 1C` 每个 CPU 一个线程编译（多模块下大幅提速）
/// - `--no-transfer-progress` 关下载进度条噪音
/// - `-Dspring-boot.repackage.skip=true` 跳过 spring-boot-maven-plugin 的 repackage
///   （原来 `mvn install` 会顺带跑 repackage，几十秒到几分钟纯浪费）
/// - `-Dmaven.test.skip=true` 完全跳过 test-compile 与 test 阶段
/// - `-Dproject.build.sourceEncoding=UTF-8` / `-Dresource.encoding=UTF-8`
///   统一资源过滤编码为 UTF-8，避免中文 Windows 默认 GBK 编码下处理含
///   UTF-8 字符的资源文件时抛 MalformedInputException（如 Nacos 控制台
///   前端资源、含中文的 application.yml 等）
pub fn common_mvn_flags() -> Vec<String> {
    vec![
        "-T".into(),
        "1C".into(),
        "--no-transfer-progress".into(),
        "-Dspring-boot.repackage.skip=true".into(),
        "-Dmaven.test.skip=true".into(),
        "-DskipTests".into(),
        "-Dproject.build.sourceEncoding=UTF-8".into(),
        "-Dresource.encoding=UTF-8".into(),
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
    // 2. 扫 src/main/java 找 @SpringBootApplication（只读文件头 16KB，够看注解和类名）
    //    注意：上面步骤 1 已解析过 pom.xml，这里只扫源码，避免重复读取
    if let Some(mc) = crate::pom::scan::scan_source_for_main_class(working_dir) {
        let _ = db::set_service_main_class(&service.id, &mc);
        return Ok(mc);
    }
    Err(AppError::Process(format!(
        "未找到主类（mainClass）：{}\n请在服务配置或 pom.xml 的 spring-boot-maven-plugin 中指定 mainClass。",
        service.name
    )))
}

/// 从 pom.xml 文本中提取主类全限定名。
///
/// 支持两种写法：
/// - `<mainClass>com.foo.App</mainClass>` 直接字面
/// - `<mainClass>${start-class}</mainClass>` + `<properties><start-class>com.foo.App</start-class></properties>`
///   （BladeX / Spring Boot Parent 约定写法），递归解引用（深度限 5）
/// - 无 mainClass 但 properties 里有 `<start-class>` →直接采用
///
/// 跳过 XML 注释。
pub fn extract_main_class_from_pom_xml(content: &str) -> Option<String> {
    // 首先尝试 <mainClass>...</mainClass>
    let mc_raw = find_tag_value(content, "mainClass");
    if let Some(v) = mc_raw {
        let resolved = resolve_pom_placeholders(&v, content, 0);
        let t = resolved.trim();
        if !t.is_empty() && !t.contains('$') {
            return Some(t.to_string());
        }
    }
    // 兜底：Spring Boot Parent 约定的 <start-class>
    if let Some(v) = find_tag_value(content, "start-class") {
        let t = v.trim();
        if !t.is_empty() && !t.contains('$') {
            return Some(t.to_string());
        }
    }
    None
}

/// 在 pom.xml 中查找第一个 `<tag>value</tag>`（跳过注释）。
fn find_tag_value(content: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let mut in_comment = false;
    let mut i = 0;
    while i < content.len() {
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
            // 按 UTF-8 字符推进，避免落在多字节字符中间导致切片 panic
            i += content[i..].chars().next().map_or(1, |c| c.len_utf8());
            continue;
        }
        if content[i..].starts_with(&open) {
            let start = i + open.len();
            if let Some(end) = content[start..].find(&close) {
                return Some(content[start..start + end].to_string());
            }
            break;
        }
        i += content[i..].chars().next().map_or(1, |c| c.len_utf8());
    }
    None
}

/// 递归解 `${name}` 占位符（深度限 5，避免循环）。
/// - 从 `<properties>` 里找 `<name>value</name>`
/// - Spring Boot Parent 历史上使用 `${start-class}` 作为“平台约定”，且 property 名包含“-”，
///   带“-”的 XML 标签能直接匹配，不需额外处理
fn resolve_pom_placeholders(input: &str, pom_content: &str, depth: u32) -> String {
    if depth >= 5 {
        return input.to_string();
    }
    let mut out = String::new();
    let mut i = 0;
    while i < input.len() {
        if input[i..].starts_with("${") {
            if let Some(end) = input[i + 2..].find('}') {
                let name = &input[i + 2..i + 2 + end];
                let raw = find_tag_value(pom_content, name)
                    .unwrap_or_else(|| format!("${{{}}}", name));
                let resolved = resolve_pom_placeholders(&raw, pom_content, depth + 1);
                out.push_str(&resolved);
                i = i + 2 + end + 1;
                continue;
            }
        }
        // 按完整 UTF-8 字符输出，避免逐字节 push 导致多字节字符损坏
        let ch = input[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
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
/// 返回 true 表示 classes 已经是最新的、可跳过编译。
/// 快路径：自动重启服务的 watcher 明确报告"干净"时，直接跳过全树 mtime 扫描
/// （大仓库数千文件的 metadata 遍历可省 100~300ms/次启动）
pub fn is_module_up_to_date(module_dir: &Path) -> bool {
    if !crate::watcher::get_watch_manager()
        .module_possibly_dirty(&module_dir.to_string_lossy())
    {
        return true;
    }
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
        let target = strip_verbatim_prefix(&working_dir.join("target"));
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
#[allow(dead_code)]
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
