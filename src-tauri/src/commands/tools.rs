//! JDK / Maven 探测：60 秒缓存 + 探测串行锁，避免重复拉起 JVM 子进程。
use std::path::PathBuf;
use std::time::{Duration, Instant};
use once_cell::sync::Lazy;
use tokio::sync::RwLock;
use crate::util::CandidateCollector;
use crate::util::NoWindow;

const TOOL_CACHE_TTL: Duration = Duration::from_secs(60);

static JDK_CACHE: Lazy<RwLock<Option<(Instant, Vec<JdkInfo>)>>> =
    Lazy::new(|| RwLock::new(None));
static MAVEN_CACHE: Lazy<RwLock<Option<(Instant, Vec<MavenInfo>)>>> =
    Lazy::new(|| RwLock::new(None));
// 探测串行锁：只保证同一类探测不重复跑，不阻塞缓存读取。
// 旧实现在慢速 JVM/Maven 探测期间持有 JDK_CACHE 写锁，导致并发调用排队数秒。
static JDK_PROBE_LOCK: Lazy<tokio::sync::Mutex<()>> =
    Lazy::new(|| tokio::sync::Mutex::new(()));
static MAVEN_PROBE_LOCK: Lazy<tokio::sync::Mutex<()>> =
    Lazy::new(|| tokio::sync::Mutex::new(()));

/// 并行探测候选路径，返回探测成功的列表
async fn detect_tools<T, F>(candidates: Vec<String>, probe: F) -> Vec<T>
where
    T: Send + 'static,
    F: Fn(&str) -> Option<T> + Send + Sync + 'static,
{
    let probe = std::sync::Arc::new(probe);
    let mut set = tokio::task::JoinSet::new();
    for c in candidates {
        let probe = probe.clone();
        set.spawn_blocking(move || probe(&c));
    }
    let mut found = vec![];
    while let Some(res) = set.join_next().await {
        if let Ok(Some(info)) = res {
            found.push(info);
        }
    }
    found
}

/// 探测系统已安装的 JDK 列表（扫描常见安装位置）
///
/// 探测期间只持有 JDK_PROBE_LOCK，不持有缓存写锁：并发读可立即命中缓存，
/// 同类探测串行化避免重复启动 JVM；`detect_mavens` 也复用这里的结果，
/// 避免 add_project 一次流程里把 JDK 探测两遍。
async fn cached_jdks() -> Vec<JdkInfo> {
    {
        let guard = JDK_CACHE.read().await;
        if let Some((ts, cached)) = guard.as_ref() {
            if ts.elapsed() < TOOL_CACHE_TTL {
                return cached.clone();
            }
        }
    }
    let _probe = JDK_PROBE_LOCK.lock().await;
    {
        let guard = JDK_CACHE.read().await;
        if let Some((ts, cached)) = guard.as_ref() {
            if ts.elapsed() < TOOL_CACHE_TTL {
                return cached.clone();
            }
        }
    }
    {
        let path = std::env::var("PATH").unwrap_or_default();
        log::info!("detect_jdks 开始: PATH={}chars", path.len());
    }
    // 候选路径收集涉及大量同步 fs IO，移入 spawn_blocking 避免阻塞 tokio runtime
    let candidates = tokio::task::spawn_blocking(collect_jdk_candidates)
        .await
        .unwrap_or_default();
    log::info!("detect_jdks: jdk 候选 {} 个: {:?}", candidates.len(), candidates);
    let mut result = detect_tools(candidates, probe_jdk).await;
    // 去重：current junction 和真实目录可能产生重复（canonicalize 后路径相同）
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    result.retain(|j| seen.insert(j.path.to_lowercase()));
    log::info!("detect_jdks: 探测到 {} 个 JDK（去重后）", result.len());
    *JDK_CACHE.write().await = Some((Instant::now(), result.clone()));
    result
}

#[tauri::command]
pub async fn detect_jdks() -> Vec<JdkInfo> {
    cached_jdks().await
}

/// 收集 JDK 候选路径（同步阻塞 IO，须在 spawn_blocking 中调用）
fn collect_jdk_candidates() -> Vec<String> {
    let mut cc = CandidateCollector::new();

    // 1. JAVA_HOME 环境变量
    if let Ok(jh) = std::env::var("JAVA_HOME") {
        cc.push(jh);
    }

    // 2. PATH 中的 java
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let java_exe = dir.join("java.exe");
            if crate::util::path_exists_follow_junction(&java_exe) {
                if let Some(bin_dir) = dir.parent() {
                    cc.push(bin_dir.to_string_lossy().to_string());
                }
            }
        }
    }

    // 3. 常见安装目录扫描
    let common_dirs = [
        r"C:\Program Files\Java",
        r"C:\Program Files\Eclipse Adoptium",
        r"C:\Program Files\Microsoft\jdk",
        r"C:\Program Files\Zulu",
        r"C:\Program Files\Amazon Corretto",
        r"C:\Program Files\BellSoft",
    ];
    if let Ok(home) = std::env::var("USERPROFILE") {
        let scoop_apps = format!("{}\\scoop\\apps", home);
        let mut scan_bases = common_dirs.to_vec();
        scan_bases.push(&scoop_apps);
        for base in &scan_bases {
            if let Ok(entries) = std::fs::read_dir(base) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_dir() {
                        if crate::util::path_exists_follow_junction(&p.join("bin").join("java.exe")) {
                            cc.push(p.to_string_lossy().to_string());
                        }
                        // scoop 下可能再嵌套一层（如 temurin17-jdk/current）：
                        // current 优先且排他——升级只替换版本目录、current 恒定，
                        // 存 current 才能保证项目配置在环境升级后长期有效
                        if let Ok(sub_entries) = std::fs::read_dir(&p) {
                            let cur = p.join("current");
                            if crate::util::path_exists_follow_junction(&cur.join("bin").join("java.exe")) {
                                cc.push(cur.to_string_lossy().to_string());
                            } else {
                                for sub in sub_entries.flatten() {
                                    let sp = sub.path();
                                    if crate::util::path_exists_follow_junction(&sp.join("bin").join("java.exe")) {
                                        cc.push(sp.to_string_lossy().to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    } else {
        for base in &common_dirs {
            if let Ok(entries) = std::fs::read_dir(base) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_dir() && crate::util::path_exists_follow_junction(&p.join("bin").join("java.exe")) {
                        cc.push(p.to_string_lossy().to_string());
                    }
                }
            }
        }
    }

    // 4. IDEA 下载的 JDK：~\.jdks\<name>\（IDE "Download JDK" 的固定落点）
    if let Ok(home) = std::env::var("USERPROFILE") {
        let jdks_dir = PathBuf::from(&home).join(".jdks");
        if let Ok(entries) = std::fs::read_dir(&jdks_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if crate::util::path_exists_follow_junction(&p.join("bin").join("java.exe")) {
                    cc.push(p.to_string_lossy().to_string());
                }
            }
        }
    }

    // 5. IDEA 注册过的 SDK 表（含手动安装后挂进 IDEA 的任意位置 JDK）
    collect_jdk_table_candidates(&mut cc);

    // 6. IDEA / Android Studio 自带 JBR（新版含 javac，可作兜底编译环境）
    for root in collect_ide_install_roots() {
        let jbr = root.join("jbr");
        if crate::util::path_exists_follow_junction(&jbr.join("bin").join("java.exe")) {
            cc.push(jbr.to_string_lossy().to_string());
        }
    }

    cc.into_candidates()
}

/// 展开 JetBrains 配置 XML 里的路径宏并统一分隔符
///
/// jdk.table.xml 中路径形如 `$USER_HOME$/.jdks/corretto-17.0.9`；
/// 其余无法解析的宏（如 $MODULE_DIR$）返回 None 跳过。
fn expand_xml_path(raw: &str) -> Option<String> {
    let mut s = raw.trim().to_string();
    if s.is_empty() {
        return None;
    }
    if s.contains("$USER_HOME$") {
        if let Ok(home) = std::env::var("USERPROFILE") {
            s = s.replace("$USER_HOME$", &home);
        }
    }
    if s.contains('$') {
        return None;
    }
    Some(s.replace('/', "\\"))
}

/// 从 jdk.table.xml 内容中提取全部 homePath 值
fn extract_home_paths(content: &str) -> Vec<String> {
    const MARKER: &str = "homePath value=\"";
    content
        .split(MARKER)
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .map(|s| s.to_string())
        .collect()
}

/// 扫描 JetBrains 系 IDE 的 SDK 注册表：
/// - `%APPDATA%\JetBrains\<IDE>\options\jdk.table.xml`（IDEA / PyCharm / GoLand 等）
/// - `%APPDATA%\Google\AndroidStudio*\options\jdk.table.xml`
fn collect_jdk_table_candidates(cc: &mut CandidateCollector) {
    let Ok(appdata) = std::env::var("APPDATA") else {
        return;
    };
    let bases = [
        PathBuf::from(&appdata).join("JetBrains"),
        PathBuf::from(&appdata).join("Google"),
    ];
    for base in &bases {
        let Ok(entries) = std::fs::read_dir(base) else {
            continue;
        };
        for entry in entries.flatten() {
            let xml = entry.path().join("options").join("jdk.table.xml");
            let Ok(content) = std::fs::read_to_string(&xml) else {
                continue;
            };
            for raw in extract_home_paths(&content) {
                let Some(path) = expand_xml_path(&raw) else {
                    continue;
                };
                let p = PathBuf::from(&path);
                if crate::util::path_exists_follow_junction(&p.join("bin").join("java.exe")) {
                    cc.push(path);
                }
            }
        }
    }
}

/// 收集 JetBrains 系 IDE 安装根目录（用于定位自带 JBR / 捆绑 Maven）
///
/// 覆盖三种安装方式：
/// - 全局安装：`C:\Program Files\JetBrains\<IDE>`
/// - Toolbox 新版（per-user）：`%LOCALAPPDATA%\Programs\<IDE>`
/// - Toolbox 旧版：`%LOCALAPPDATA%\JetBrains\Toolbox\apps\<app>\<channel>\<version>`
fn collect_ide_install_roots() -> Vec<PathBuf> {
    fn name_matches(name: &str) -> bool {
        let n = name.to_lowercase();
        n.contains("intellij") || n.contains("idea") || n.contains("android") || n.contains("jetbrains")
    }

    let mut roots: Vec<PathBuf> = Vec::new();

    // C:\Program Files\JetBrains\*
    if let Ok(entries) = std::fs::read_dir(r"C:\Program Files\JetBrains") {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() && name_matches(&entry.file_name().to_string_lossy()) {
                roots.push(p);
            }
        }
    }

    if let Some(local) = dirs::data_local_dir() {
        // %LOCALAPPDATA%\Programs\<IDE>
        let programs = local.join("Programs");
        if let Ok(entries) = std::fs::read_dir(&programs) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() && name_matches(&entry.file_name().to_string_lossy()) {
                    roots.push(p);
                }
            }
        }
        // %LOCALAPPDATA%\JetBrains\Toolbox\apps\<app>\<ver>（旧版布局两层）
        let toolbox_apps = local.join("JetBrains").join("Toolbox").join("apps");
        if let Ok(apps) = std::fs::read_dir(&toolbox_apps) {
            for app in apps.flatten() {
                if let Ok(channels) = std::fs::read_dir(app.path()) {
                    for channel in channels.flatten() {
                        if channel.path().is_dir() {
                            roots.push(channel.path());
                        }
                    }
                }
            }
        }
    }

    roots
}

#[derive(Clone, serde::Serialize)]
pub struct JdkInfo {
    pub path: String,
    pub version: String,
    pub vendor: String,
}

/// 探测某 JDK_HOME 的版本信息
fn probe_jdk(java_home: &str) -> Option<JdkInfo> {
    let java_exe = std::path::PathBuf::from(java_home).join("bin").join("java.exe");
    // 用 metadata 跟随 junction，避免 scoop current 链接在 elevated 进程中无法解析
    if !crate::util::path_exists_follow_junction(&java_exe) {
        // 尝试 canonicalize 解析 junction 后再检查
        let resolved = crate::util::resolve_junction(std::path::Path::new(java_home));
        let resolved_exe = resolved.join("bin").join("java.exe");
        if !crate::util::path_exists_follow_junction(&resolved_exe) {
            log::warn!("probe_jdk: {} 不存在 java.exe (resolved: {})", java_home, resolved.display());
            return None;
        }
    }
    // 用 canonicalize 解析后的路径执行，避免 junction 解析问题
    let real_exe = crate::util::canonicalize_clean(&java_exe).unwrap_or_else(|| java_exe.clone());
    let output = std::process::Command::new(&real_exe)
        .arg("-version")
        .creation_flags_no_window()
        .output();
    let output = match output {
        Ok(o) => o,
        Err(e) => {
            log::warn!("probe_jdk: 执行 {} -version 失败: {}", real_exe.display(), e);
            return None;
        }
    };
    if !output.status.success() {
        log::warn!(
            "probe_jdk: {} -version 退出码 {:?}, stderr: {}",
            java_exe.display(),
            output.status.code(),
            crate::util::decode_output(&output.stderr)
        );
    }
    // java -version 输出到 stderr
    let text = crate::util::decode_output(&output.stderr);
    let mut version = String::from("unknown");
    let mut vendor = String::from("unknown");
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with("openjdk version") || l.starts_with("java version") {
            if let Some(start) = l.find('"') {
                if let Some(end) = l[start + 1..].find('"') {
                    version = l[start + 1..start + 1 + end].to_string();
                }
            }
        }
        if l.contains("OpenJDK") || l.contains("Oracle") || l.contains("Temurin")
            || l.contains("Zulu") || l.contains("Corretto") || l.contains("Microsoft")
            || l.contains("Liberica") || l.contains("GraalVM")
        {
            // 取第一个含 vendor 关键字的词
            for word in l.split_whitespace() {
                let lower = word.to_lowercase();
                if lower.contains("openjdk") || lower.contains("temurin")
                    || lower.contains("zulu") || lower.contains("corretto")
                    || lower.contains("microsoft") || lower.contains("oracle")
                    || lower.contains("liberica") || lower.contains("graalvm")
                {
                    vendor = word.trim_matches(|c: char| !c.is_alphanumeric()).to_string();
                    break;
                }
            }
        }
    }
    // 保留候选原路径（scoop 场景即 ...\current junction）：
    // junction 升级后指向自动跟随，存版本化真实目录反而会在 scoop 清理旧版本后失效。
    // 运行层使用时由 resolve_java_home / canonicalize_clean 统一解析，无需在此固化。
    Some(JdkInfo {
        path: java_home.to_string(),
        version,
        vendor,
    })
}

#[derive(Clone, serde::Serialize)]
pub struct MavenInfo {
    pub path: String,
    pub version: String,
}

/// 探测系统已安装的 Maven 列表
#[tauri::command]
pub async fn detect_mavens() -> Vec<MavenInfo> {
    {
        let guard = MAVEN_CACHE.read().await;
        if let Some((ts, cached)) = guard.as_ref() {
            if ts.elapsed() < TOOL_CACHE_TTL {
                return cached.clone();
            }
        }
    }
    let _probe = MAVEN_PROBE_LOCK.lock().await;
    {
        let guard = MAVEN_CACHE.read().await;
        if let Some((ts, cached)) = guard.as_ref() {
            if ts.elapsed() < TOOL_CACHE_TTL {
                return cached.clone();
            }
        }
    }
    // 诊断：记录 detect_mavens 调用时的环境状态
    {
        let path = std::env::var("PATH").unwrap_or_default();
        let java_home = std::env::var("JAVA_HOME").unwrap_or_default();
        log::info!(
            "detect_mavens 开始: PATH={}chars, JAVA_HOME={}",
            path.len(),
            if java_home.is_empty() { "(空)" } else { &java_home }
        );
    }
    // 候选路径收集涉及大量同步 fs IO，移入 spawn_blocking 避免阻塞 tokio runtime
    let candidates = tokio::task::spawn_blocking(collect_maven_candidates)
        .await
        .unwrap_or_default();
    log::info!("detect_mavens: maven 候选 {} 个: {:?}", candidates.len(), candidates);
    // 复用 cached_jdks：命中 60s 缓存时不再重复拉起 java -version 子进程。
    // （安装器启动时 PATH 不完整，probe_maven 内部 which_java() 可能失败）
    let jdks = cached_jdks().await;
    let fallback_java_home = jdks.first().map(|j| j.path.clone());
    log::info!("detect_mavens: 使用 {} 个已探测 JDK 作为回退", jdks.len());
    let mut result = detect_tools(candidates, move |m| probe_maven(m, fallback_java_home.as_deref())).await;
    let mut seen_mvn: std::collections::HashSet<String> = std::collections::HashSet::new();
    result.retain(|m| seen_mvn.insert(m.path.to_lowercase()));
    log::info!("detect_mavens: 探测到 {} 个 maven（去重后）: {:?}", result.len(), result.iter().map(|m| (&m.path, &m.version)).collect::<Vec<_>>());
    *MAVEN_CACHE.write().await = Some((Instant::now(), result.clone()));
    result
}

/// 收集 Maven 候选路径（同步阻塞 IO，须在 spawn_blocking 中调用）
fn collect_maven_candidates() -> Vec<String> {
    let mut cc = CandidateCollector::new();

    // 1. MAVEN_HOME / M2_HOME 环境变量
    for var in ["MAVEN_HOME", "M2_HOME"] {
        if let Ok(mh) = std::env::var(var) {
            cc.push(mh);
        }
    }

    // 2. PATH 中的 mvn
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let mvn_exe = dir.join("mvn.cmd");
            if crate::util::path_exists_follow_junction(&mvn_exe) {
                if let Some(bin_dir) = dir.parent() {
                    cc.push(bin_dir.to_string_lossy().to_string());
                }
            }
        }
    }

    // 3. scoop 安装目录：current junction 优先且排他（升级恒定，配置长期有效），
    //    无 current 时退回扫描版本目录
    if let Ok(home) = std::env::var("USERPROFILE") {
        let scoop_current = format!("{}\\scoop\\apps\\maven\\current", home);
        let scoop_current_mvn = std::path::Path::new(&scoop_current).join("bin").join("mvn.cmd");
        if crate::util::path_exists_follow_junction(&scoop_current_mvn) {
            cc.push(scoop_current);
        } else {
            let scoop_maven = format!("{}\\scoop\\apps\\maven", home);
            if let Ok(entries) = std::fs::read_dir(&scoop_maven) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if crate::util::path_exists_follow_junction(&p.join("bin").join("mvn.cmd")) {
                        cc.push(p.to_string_lossy().to_string());
                    }
                }
            }
        }
    }

    // 4. 常见安装目录
    let common_dirs = [
        r"C:\Program Files\Apache\maven",
        r"C:\Program Files\Maven",
        r"C:\apache-maven",
    ];
    for base in &common_dirs {
        if let Ok(entries) = std::fs::read_dir(base) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() && crate::util::path_exists_follow_junction(&p.join("bin").join("mvn.cmd")) {
                    cc.push(p.to_string_lossy().to_string());
                }
            }
        }
    }

    // 5. IDEA 捆绑的 Maven：<IDE 安装目录>\plugins\maven\lib\maven3
    //    （项目设置里选 "Use Maven home: Bundled (Maven 3)" 时实际使用的就是它）
    for root in collect_ide_install_roots() {
        let m3 = root.join("plugins").join("maven").join("lib").join("maven3");
        if crate::util::path_exists_follow_junction(&m3.join("bin").join("mvn.cmd")) {
            cc.push(m3.to_string_lossy().to_string());
        }
    }

    // 6. Maven Wrapper 下载的分发包：~\.m2\wrapper\dists\<dist>\<hash>\<apache-maven-x.y.z>
    //    （项目首次跑 mvnw.cmd 时解压到此处，是现成可用的完整发行版）
    if let Ok(home) = std::env::var("USERPROFILE") {
        let dists = PathBuf::from(&home).join(".m2").join("wrapper").join("dists");
        if let Ok(l1) = std::fs::read_dir(&dists) {
            for d1 in l1.flatten() {
                let Ok(l2) = std::fs::read_dir(d1.path()) else { continue };
                for d2 in l2.flatten() {
                    let Ok(l3) = std::fs::read_dir(d2.path()) else { continue };
                    for d3 in l3.flatten() {
                        let p = d3.path();
                        if crate::util::path_exists_follow_junction(&p.join("bin").join("mvn.cmd")) {
                            cc.push(p.to_string_lossy().to_string());
                        }
                    }
                }
            }
        }
    }

    cc.into_candidates()
}

/// 探测某 MAVEN_HOME 的版本信息
/// `fallback_java_home`: 由 detect_mavens 传入的已检测到的 JDK 路径，用于 PATH 不完整时
fn probe_maven(maven_home: &str, fallback_java_home: Option<&str>) -> Option<MavenInfo> {
    let mvn_cmd = std::path::PathBuf::from(maven_home).join("bin").join("mvn.cmd");
    if !crate::util::path_exists_follow_junction(&mvn_cmd) {
        log::warn!("probe_maven: {} 不存在 mvn.cmd", maven_home);
        return None;
    }
    // canonicalize 解析 junction，避免 elevated 进程无法解析 scoop current 链接
    let mvn_cmd_real = crate::util::canonicalize_clean(&mvn_cmd).unwrap_or_else(|| mvn_cmd.clone());
    // mvn.cmd 强制要求 JAVA_HOME；优先用系统 JAVA_HOME，否则 fallback 到传入的 JDK 路径，
    // 最后尝试 which_java() 反推（PATH 不完整时可能失败）
    let java_home = std::env::var("JAVA_HOME").ok().filter(|s| !s.is_empty());
    let java_home = if java_home.is_some() {
        java_home
    } else if let Some(fbh) = fallback_java_home {
        Some(fbh.to_string())
    } else {
        // JAVA_HOME 未设置时，从 PATH 里的 java.exe 反推 JAVA_HOME
        crate::process::env::which_java().and_then(|java_exe| {
            std::path::PathBuf::from(java_exe)
                .parent() // bin
                .and_then(|bin| bin.parent()) // java_home
                .map(|p| p.to_string_lossy().to_string())
        })
    };
    // canonicalize JAVA_HOME，避免 junction 解析失败导致 mvn.cmd 找不到 java
    let java_home = java_home.map(|jh| {
        crate::util::canonicalize_clean(std::path::Path::new(&jh))
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or(jh)
    });

    let mut cmd = std::process::Command::new("cmd");
    cmd.arg("/c").arg(&mvn_cmd_real).arg("-v");
    if let Some(jh) = &java_home {
        cmd.env("JAVA_HOME", jh);
        let cur_path = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{}\\bin;{}", jh, cur_path));
    }
    let output = cmd.creation_flags_no_window().output();
    let output = match output {
        Ok(o) => o,
        Err(e) => {
            log::warn!("probe_maven: 执行 {} -v 失败: {}", mvn_cmd.display(), e);
            return None;
        }
    };
    // mvn.cmd 失败时错误信息在 stderr，stdout 为空；合并两者避免漏掉
    let stdout = crate::util::decode_output(&output.stdout);
    let stderr = crate::util::decode_output(&output.stderr);
    // 优先取 stdout 第一行（正常 "Apache Maven 3.9.6 ..."），fallback 到 stderr
    let text = if stdout.trim().is_empty() { &stderr } else { &stdout };
    let version = text.lines().next().unwrap_or("unknown").trim().to_string();
    // 如果版本信息含错误关键字，视为探测失败
    if version.is_empty()
        || version.contains("not found")
        || version.contains("ERROR")
        || version.contains("Exception")
    {
        log::warn!(
            "probe_maven: {} -v 探测失败，stdout={}, stderr={}",
            mvn_cmd.display(),
            stdout.trim(),
            stderr.trim()
        );
        return None;
    }
    Some(MavenInfo {
        path: maven_home.to_string(),
        version,
    })
}

