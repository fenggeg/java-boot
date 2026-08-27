//! 环境解析 & 命令定位
//!
//! 从原 manager.rs 抽离：
//! - `EnvConfig` / `resolve_env_config` / `resolve_maven_cmd` / `resolve_java_home`
//! - `preflight_check` / `which_java` / `which_mvn`（PATH + scoop shims fallback，不缓存）
//! - `inject_env`：给子进程注入 JAVA_HOME / MAVEN_HOME / PATH
//!   （去掉了 `env_clear + 复制全部环境变量` 的反模式，直接 override 需要的 key）

use std::path::{Path, PathBuf};

use tokio::process::Command;

use crate::db;
use crate::db::models::Service;
use crate::error::AppResult;
use crate::util::NoWindow;

/// 环境配置（从项目解析得出）
#[derive(Clone)]
pub struct EnvConfig {
    pub java_home: Option<String>,
    pub maven_home: Option<String>,
    /// 项目根路径（用于多模块 install）
    pub project_root: Option<String>,
    /// 自定义环境变量（项目级 + 服务级合并，服务级覆盖项目级同名 key）
    /// 在 JAVA_HOME/MAVEN_HOME/PATH/MAVEN_OPTS 之后注入，可覆盖内置变量
    pub env_vars: Vec<(String, String)>,
}

/// 从服务的 project_id 查项目，解析出项目级 JDK / Maven / 环境变量配置
pub fn resolve_env_config(service: &Service) -> AppResult<EnvConfig> {
    let mut cfg = EnvConfig {
        java_home: None,
        maven_home: None,
        project_root: None,
        env_vars: Vec::new(),
    };
    if let Some(pid) = &service.project_id {
        if let Ok(project) = db::get_project(pid) {
            cfg.java_home = project.java_home.and_then(non_empty);
            cfg.maven_home = project.maven_home.and_then(non_empty);
            cfg.project_root = Some(project.root_path);
            // 项目级环境变量
            cfg.env_vars = parse_env_vars(&project.env_vars);
        }
    }
    // 服务级环境变量覆盖项目级同名 key
    let service_env = parse_env_vars(&service.env_vars);
    if !service_env.is_empty() {
        for (k, v) in service_env {
            if let Some(entry) = cfg.env_vars.iter_mut().find(|(ek, _)| ek == &k) {
                entry.1 = v;
            } else {
                cfg.env_vars.push((k, v));
            }
        }
    }
    Ok(cfg)
}

/// 解析环境变量 JSON：`[{"key":"FOO","value":"bar"}]` → `[(FOO, bar)]`
/// 跳过 key 为空或非字符串的条目；value 缺失视为空串
pub fn parse_env_vars(json: &Option<String>) -> Vec<(String, String)> {
    let Some(s) = json.as_deref() else { return Vec::new() };
    let s = s.trim();
    if s.is_empty() {
        return Vec::new();
    }
    let parsed: Result<Vec<EnvVarEntry>, _> = serde_json::from_str(s);
    let Ok(arr) = parsed else { return Vec::new() };
    arr.into_iter()
        .filter(|e| !e.key.trim().is_empty())
        .map(|e| (e.key.trim().to_string(), e.value))
        .collect()
}

#[derive(serde::Deserialize)]
struct EnvVarEntry {
    key: String,
    value: String,
}

fn non_empty(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// 解析 Maven 命令：项目 maven_home > 项目 mvnw.cmd > mvnw.bat > mvnw > 系统 mvn
pub fn resolve_maven_cmd(working_dir: &Path, cfg: &EnvConfig) -> (String, Vec<String>) {
    if let Some(mh) = &cfg.maven_home {
        let mvn_cmd = PathBuf::from(mh).join("bin").join("mvn.cmd");
        if crate::util::path_exists_follow_junction(&mvn_cmd) {
            let real = crate::util::canonicalize_clean(&mvn_cmd).unwrap_or_else(|| mvn_cmd);
            return (
                "cmd".to_string(),
                vec!["/c".to_string(), real.to_string_lossy().to_string()],
            );
        }
        let mvn_bin = PathBuf::from(mh).join("bin").join("mvn");
        if crate::util::path_exists_follow_junction(&mvn_bin) {
            let real = crate::util::canonicalize_clean(&mvn_bin).unwrap_or_else(|| mvn_bin);
            return (real.to_string_lossy().to_string(), vec![]);
        }
        log::warn!("项目配置的 maven_home 无效: {}", mh);
    }
    let mvnw_cmd = working_dir.join("mvnw.cmd");
    let mvnw_bat = working_dir.join("mvnw.bat");
    let mvnw = working_dir.join("mvnw");
    if mvnw_cmd.exists() {
        (
            "cmd".to_string(),
            vec!["/c".to_string(), mvnw_cmd.to_string_lossy().to_string()],
        )
    } else if mvnw_bat.exists() {
        (
            "cmd".to_string(),
            vec!["/c".to_string(), mvnw_bat.to_string_lossy().to_string()],
        )
    } else if mvnw.exists() {
        (mvnw.to_string_lossy().to_string(), vec![])
    } else {
        ("mvn".to_string(), vec![])
    }
}

/// 校验 JAVA_HOME 是否有效：bin/java.exe 必须存在。
///
/// 背景：mvn.cmd 会检查 `%JAVA_HOME%\bin\java.exe`，JAVA_HOME 指向已卸载/迁移的
/// JDK 时报 "The JAVA_HOME environment variable is not defined correctly"，
/// 而 PATH 里若有可用 java，preflight 会误放行。注入前必须先验证。
fn java_home_valid(home: &str) -> bool {
    crate::util::path_exists_follow_junction(
        &PathBuf::from(home).join("bin").join("java.exe"),
    )
}

/// 反推缓存：java.exe 路径 → 真实 java.home（进程内共享，避免批量启动时反复探测）
/// key 为 java.exe 绝对路径，种类有限（PATH 中条目），不会无界增长；
/// 设 64 条上限做防御性保护
static JAVA_HOME_DETECT_CACHE: once_cell::sync::Lazy<
    parking_lot::Mutex<std::collections::HashMap<String, Option<String>>>,
> = once_cell::sync::Lazy::new(|| parking_lot::Mutex::new(std::collections::HashMap::new()));
const JAVA_HOME_CACHE_MAX: usize = 64;

/// Java 版本缓存：java.exe 路径 → 主版本号（进程内共享）
/// 与 JAVA_HOME_DETECT_CACHE 同理，避免批量启动时反复执行 java -version
static JAVA_VERSION_CACHE: once_cell::sync::Lazy<
    parking_lot::Mutex<std::collections::HashMap<String, Option<u32>>>,
> = once_cell::sync::Lazy::new(|| parking_lot::Mutex::new(std::collections::HashMap::new()));
const JAVA_VERSION_CACHE_MAX: usize = 64;

/// 通过执行 java 反推真实 java.home（处理 PATH 目录、scoop shims 等非标准布局）
///
/// scoop 的 shims/java.exe 是转发 stub，其所在目录不是 JDK home；
/// 用 `-XshowSettings:properties -version` 输出中的 `java.home` 拿到真实路径。
fn detect_java_home_from_java_exe(java_exe: &str) -> Option<String> {
    let mut cache = JAVA_HOME_DETECT_CACHE.lock();
    if let Some(cached) = cache.get(java_exe).cloned() {
        return cached;
    }
    // 防御性清理：缓存条目超过上限时清空（key 种类有限，正常不会触发）
    if cache.len() >= JAVA_HOME_CACHE_MAX {
        cache.clear();
    }
    drop(cache);
    let detected = detect_java_home_uncached(java_exe);
    log::info!(
        "从 {} 反推 java.home: {:?}",
        java_exe,
        detected.as_deref().unwrap_or("<失败>")
    );
    JAVA_HOME_DETECT_CACHE
        .lock()
        .insert(java_exe.to_string(), detected.clone());
    detected
}

fn detect_java_home_uncached(java_exe: &str) -> Option<String> {
    let output = std::process::Command::new(java_exe)
        .args(["-XshowSettings:properties", "-version"])
        .creation_flags_no_window()
        .output()
        .ok()?;
    // 属性输出在 stderr；合并 stdout 兜底
    let mut text = crate::util::decode_output(&output.stderr);
    text.push_str(&crate::util::decode_output(&output.stdout));
    for line in text.lines() {
        // 形如 "    java.home = C:\Program Files\Java\jdk-17"
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("java.home") {
            let rest = rest.trim_start();
            if let Some(v) = rest.strip_prefix('=') {
                let home = v.trim();
                if home.is_empty() {
                    continue;
                }
                let canon = |p: &Path| {
                    crate::util::canonicalize_clean(p)
                        .map(|x| x.to_string_lossy().to_string())
                        .unwrap_or_else(|| p.to_string_lossy().to_string())
                };
                // JDK ≤ 9 的 java.home 指向 <JDK>\jre 子目录：
                // 父目录含 bin/javac.exe 时优先用父目录，否则 Maven 编译找不到 tools.jar
                if let Some(parent) = Path::new(home).parent() {
                    if java_home_valid(&parent.to_string_lossy())
                        && crate::util::path_exists_follow_junction(
                            &parent.join("bin").join("javac.exe"),
                        )
                    {
                        return Some(canon(parent));
                    }
                }
                if java_home_valid(home) {
                    return Some(canon(Path::new(home)));
                }
            }
        }
    }
    None
}

/// 确定生效的 JAVA_HOME，逐级回退并校验有效性：
/// 1. 项目配置的 java_home
/// 2. 系统环境变量 JAVA_HOME
/// 3. 从 PATH / scoop shims 里的 java.exe 反推真实 home
///
/// 每个候选都会 canonicalize 解析 junction（scoop current 在 elevated 进程中
/// 可能无法解析），且要求 bin/java.exe 存在——无效配置直接跳过而不是注入给 mvn。
pub fn resolve_java_home(cfg: &EnvConfig) -> Option<String> {
    let candidates = [
        cfg.java_home.clone(),
        std::env::var("JAVA_HOME").ok().filter(|s| !s.is_empty()),
    ];
    for raw in candidates.into_iter().flatten() {
        if raw.trim().is_empty() {
            continue;
        }
        let resolved = crate::util::canonicalize_clean(Path::new(&raw))
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or(raw);
        if java_home_valid(&resolved) {
            return Some(resolved);
        }
        log::warn!("跳过无效 JAVA_HOME: {}（bin\\java.exe 不存在）", resolved);
    }

    // 全部候选无效时反推
    let java_exe = which_java()?;
    let home = detect_java_home_from_java_exe(&java_exe);
    if home.is_none() {
        log::warn!(
            "无法从 {} 反推有效 java.home，项目配置与系统 JAVA_HOME 均无效",
            java_exe
        );
    }
    home
}

/// 检测 Java 主版本号（如 8, 11, 17, 21）。
///
/// `@argfile` 是 JDK 9 引入的功能（JEP 294），JDK 8 不支持。
/// 此函数用于在命令行超长时决定是否可用 @argfile，还是需要改用 CLASSPATH 环境变量方案。
///
/// 解析 `java -version` 输出：
/// - JDK ≤ 8: `version "1.8.0_302"` → 主版本取第二段 (8)
/// - JDK ≥ 9: `version "17.0.2"`   → 主版本取第一段 (17)
pub fn detect_java_major_version(cfg: &EnvConfig) -> Option<u32> {
    let java_home = resolve_java_home(cfg)?;
    let java_exe = format!("{}\\bin\\java.exe", java_home);
    detect_java_major_version_from_exe(&java_exe)
}

/// 从 java.exe 路径检测 Java 主版本号（带缓存）
fn detect_java_major_version_from_exe(java_exe: &str) -> Option<u32> {
    let mut cache = JAVA_VERSION_CACHE.lock();
    if let Some(cached) = cache.get(java_exe).cloned() {
        return cached;
    }
    if cache.len() >= JAVA_VERSION_CACHE_MAX {
        cache.clear();
    }
    drop(cache);

    let detected = detect_java_major_version_uncached(java_exe);
    log::info!(
        "Java 版本检测: {} → major {}",
        java_exe,
        detected.map_or("<失败>".to_string(), |v| v.to_string())
    );
    JAVA_VERSION_CACHE
        .lock()
        .insert(java_exe.to_string(), detected);
    detected
}

fn detect_java_major_version_uncached(java_exe: &str) -> Option<u32> {
    let output = std::process::Command::new(java_exe)
        .arg("-version")
        .creation_flags_no_window()
        .output()
        .ok()?;
    // -version 输出在 stderr
    let text = crate::util::decode_output(&output.stderr);
    // 形如: openjdk version "1.8.0_302"  或  openjdk version "17.0.2"
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.find('"') {
            let after_quote = &t[rest + 1..];
            if let Some(end) = after_quote.find('"') {
                let version_str = &after_quote[..end];
                return parse_java_major_version(version_str);
            }
        }
    }
    None
}

/// 解析 Java 版本字符串为主版本号
/// "1.8.0_302" → 8, "17.0.2" → 17, "21" → 21
fn parse_java_major_version(version: &str) -> Option<u32> {
    let first_part = version.split('.').next()?;
    if first_part == "1" {
        // JDK ≤ 8: 1.8.x → 8
        version.split('.').nth(1)?.split('_').next()?.parse().ok()
    } else {
        first_part.parse().ok()
    }
}

/// 启动前预检：确认 java / mvn 可用
pub fn preflight_check(
    cfg: &EnvConfig,
    working_dir: &Path,
    program: &str,
) -> AppResult<()> {
    // 1. java 可执行性检查
    // resolve_java_home 已做有效性校验与多级回退，返回 None 说明所有来源均无效，
    // 此时即使 PATH 里残留 java.exe（反推也失败），mvn / java 也无法正常工作，直接报错
    let java_home = resolve_java_home(cfg);
    if java_home.is_none() {
        return Err(crate::error::AppError::Process(
            "未找到可用的 JDK。\n项目配置的 JDK 与系统 JAVA_HOME 均无效（缺少 bin\\java.exe），且无法从 PATH 中的 java 反推。\n请在项目设置里指定正确的 JDK 路径，或修复系统 JAVA_HOME。".to_string(),
        ));
    }

    // 2. mvn 可执行性检查
    if let Some(mh) = &cfg.maven_home {
        let mvn_cmd = PathBuf::from(mh).join("bin").join("mvn.cmd");
        let mvn_bin = PathBuf::from(mh).join("bin").join("mvn");
        if !mvn_cmd.exists() && !mvn_bin.exists() {
            return Err(crate::error::AppError::Process(format!(
                "项目配置的 Maven 路径无效: {}（未找到 bin/mvn.cmd）",
                mh
            )));
        }
        return Ok(());
    }
    let using_mvnw = program == "cmd"
        || working_dir.join("mvnw").exists()
        || working_dir.join("mvnw.cmd").exists()
        || working_dir.join("mvnw.bat").exists();
    if !using_mvnw && which_mvn().is_none() {
        return Err(crate::error::AppError::Process(
            "未找到 mvn 命令。\n请安装 Maven 并加入 PATH，或在项目根目录放置 mvnw.cmd，或在项目设置里指定 Maven 路径。".to_string(),
        ));
    }
    Ok(())
}

/// 在 PATH 中查找 java（不缓存，因为安装器启动时 PATH 可能不完整）
pub fn which_java() -> Option<String> {
    // 1. PATH 中的 java
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("java.exe");
        if crate::util::path_exists_follow_junction(&candidate) {
            return Some(candidate.to_string_lossy().to_string());
        }
    }
    // 2. scoop shims（安装器启动时可能不继承用户 PATH）
    if let Ok(home) = std::env::var("USERPROFILE") {
        let shim = format!("{}\\scoop\\shims\\java.exe", home);
        if crate::util::path_exists_follow_junction(std::path::Path::new(&shim)) {
            return Some(shim);
        }
    }
    None
}

/// 在 PATH 中查找 mvn（不缓存，因为安装器启动时 PATH 可能不完整）
pub fn which_mvn() -> Option<String> {
    // 1. PATH 中的 mvn
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("mvn.cmd");
            if crate::util::path_exists_follow_junction(&candidate) {
                return Some(candidate.to_string_lossy().to_string());
            }
            let candidate = dir.join("mvn");
            if crate::util::path_exists_follow_junction(&candidate) {
                return Some(candidate.to_string_lossy().to_string());
            }
        }
    }
    // 2. scoop shims（安装器启动时可能不继承用户 PATH）
    if let Ok(home) = std::env::var("USERPROFILE") {
        let shim = format!("{}\\scoop\\shims\\mvn.exe", home);
        if crate::util::path_exists_follow_junction(std::path::Path::new(&shim)) {
            return Some(shim);
        }
    }
    None
}

// ================================================================
// 环境变量注入
// ================================================================

/// 抽象两种 Command 的环境注入能力，统一 tokio / std 实现
pub trait CmdEnv {
    fn set_env(&mut self, k: &str, v: &str);
}
impl CmdEnv for Command {
    fn set_env(&mut self, k: &str, v: &str) {
        self.env(k, v);
    }
}
impl CmdEnv for std::process::Command {
    fn set_env(&mut self, k: &str, v: &str) {
        self.env(k, v);
    }
}

/// 为子进程注入环境变量：先注入 JAVA_HOME / MAVEN_HOME / PATH / MAVEN_OPTS，
/// 再注入用户自定义环境变量（项目级 + 服务级合并），后者可覆盖前者。
/// （原实现 env_clear + 全量复制 std::env::vars() 是负优化，`Command` 默认就继承父进程）
pub fn inject_env<C: CmdEnv>(cmd: &mut C, cfg: &EnvConfig) {
    let mut path_prefix = String::new();
    if let Some(jh) = resolve_java_home(cfg) {
        cmd.set_env("JAVA_HOME", &jh);
        path_prefix = format!("{}\\bin;", jh);
    }
    if let Some(mh) = &cfg.maven_home {
        cmd.set_env("MAVEN_HOME", mh);
        cmd.set_env("M2_HOME", mh);
        path_prefix = format!("{}{}\\bin;", path_prefix, mh);
    }
    if !path_prefix.is_empty() {
        let cur_path = std::env::var("PATH").unwrap_or_default();
        cmd.set_env("PATH", &format!("{}{}", path_prefix, cur_path));
    }
    // Maven 子进程堆与编码：大项目默认堆可能不足导致编译期频繁 GC。
    // 仅对 mvn 生效（java 直启忽略 MAVEN_OPTS）；保留用户已设置的值， ours 追加在后优先
    const MAVEN_OPTS_BASE: &str = "-Xmx1g -Dfile.encoding=UTF-8";
    let merged = match std::env::var("MAVEN_OPTS") {
        Ok(v) if !v.trim().is_empty() => format!("{} {}", v.trim(), MAVEN_OPTS_BASE),
        _ => MAVEN_OPTS_BASE.to_string(),
    };
    cmd.set_env("MAVEN_OPTS", &merged);

    // 用户自定义环境变量（项目级 + 服务级），在内置变量之后注入，可覆盖 JAVA_HOME/PATH 等
    for (k, v) in &cfg.env_vars {
        cmd.set_env(k, v);
    }
}

// ================================================================
// 注册表 PATH 合并（修复安装器启动时 PATH 不完整问题）
// ================================================================

/// 从 Windows 注册表读取系统级和用户级 PATH，合并到当前进程环境变量。
///
/// **背景**：NSIS/MSI 安装器勾选"同时打开"启动应用时，子进程可能不继承用户级 PATH
/// （特别是 scoop、用户手动添加的 JDK/Maven 路径），导致 `which_java`/`which_mvn`/
/// `resolve_git` 全部失败。从注册表读取完整 PATH 并合并后，所有后续检测恢复正常。
///
/// 仅 Windows 调用，在 app setup 最早期执行。
#[cfg(windows)]
pub fn merge_registry_path() {
    use std::os::windows::ffi::OsStringExt;

    // 读取注册表字符串（REG_EXPAND_SZ / REG_SZ）
    fn read_reg(
        hive: u32,
        subkey: &str,
        value: &str,
    ) -> Option<String> {
        // 延迟绑定 winapi，避免在非 windows 平台编译失败
        extern "system" {
            fn RegOpenKeyExW(
                hkey: u32,
                lpsubkey: *const u16,
                uloptions: u32,
                samdesired: u32,
                phkresult: *mut u32,
            ) -> i32;
            fn RegQueryValueExW(
                hkey: u32,
                lpvaluename: *const u16,
                lpreserved: *const u32,
                lptype: *mut u32,
                lpdata: *mut u8,
                lpcbdata: *mut u32,
            ) -> i32;
            fn RegCloseKey(hkey: u32) -> i32;
        }

        const HKEY_LOCAL_MACHINE: u32 = 0x80000002;
        const HKEY_CURRENT_USER: u32 = 0x80000001;
        const KEY_READ: u32 = 0x20019;

        let _ = (HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER); // 抑制未使用警告

        let subkey_w: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
        let value_w: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();

        let mut hkey: u32 = 0;
        let rc = unsafe {
            RegOpenKeyExW(hive, subkey_w.as_ptr(), 0, KEY_READ, &mut hkey)
        };
        if rc != 0 {
            return None;
        }

        // 先查长度
        let mut len: u32 = 0;
        let mut typ: u32 = 0;
        let rc = unsafe {
            RegQueryValueExW(hkey, value_w.as_ptr(), std::ptr::null(), &mut typ, std::ptr::null_mut(), &mut len)
        };
        if rc != 0 || len == 0 {
            unsafe { RegCloseKey(hkey); }
            return None;
        }

        let mut buf = vec![0u8; len as usize];
        let rc = unsafe {
            RegQueryValueExW(hkey, value_w.as_ptr(), std::ptr::null(), &mut typ, buf.as_mut_ptr(), &mut len)
        };
        unsafe { RegCloseKey(hkey); }
        if rc != 0 {
            return None;
        }

        // REG_SZ(1) / REG_EXPAND_SZ(2) 都是 UTF-16LE，末尾含 null
        let nchars = (len as usize) / 2;
        let wchars: Vec<u16> = (0..nchars)
            .map(|i| (buf[i * 2] as u16) | ((buf[i * 2 + 1] as u16) << 8))
            .collect();
        let trimmed: Vec<u16> = wchars.into_iter().take_while(|&c| c != 0).collect();
        let s = std::ffi::OsString::from_wide(&trimmed)
            .to_string_lossy()
            .into_owned();
        Some(s)
    }

    const HKEY_LOCAL_MACHINE: u32 = 0x80000002;
    const HKEY_CURRENT_USER: u32 = 0x80000001;

    let sys_path = read_reg(
        HKEY_LOCAL_MACHINE,
        r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
        "Path",
    );
    let user_path = read_reg(
        HKEY_CURRENT_USER,
        r"Environment",
        "Path",
    );

    let cur_path = std::env::var("PATH").unwrap_or_default();

    // 合并：当前进程 PATH + 注册表系统 PATH + 注册表用户 PATH，去重
    let mut all_dirs: Vec<std::path::PathBuf> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for source in [&cur_path, &sys_path.unwrap_or_default(), &user_path.unwrap_or_default()] {
        for dir in std::env::split_paths(source) {
            let key = dir.to_string_lossy().to_lowercase();
            if !key.is_empty() && seen.insert(key) {
                all_dirs.push(dir);
            }
        }
    }

    let merged = std::env::join_paths(all_dirs).unwrap_or_else(|_| cur_path.clone().into());
    let merged_str = merged.to_string_lossy().to_string();

    if merged_str != cur_path {
        log::info!("注册表 PATH 合并：{} -> {} chars", cur_path.len(), merged_str.len());
        std::env::set_var("PATH", merged_str);
    }

    // 同时补齐 JAVA_HOME / MAVEN_HOME（如果注册表 Environment 里有但当前进程没有）
    // canonicalize JAVA_HOME（无条件）：如果 JAVA_HOME 指向 scoop current junction，
    // elevated 进程可能无法解析，canonicalize 为真实路径
    if let Ok(jh) = std::env::var("JAVA_HOME") {
        if !jh.is_empty() {
            if let Some(real) = crate::util::canonicalize_clean(std::path::Path::new(&jh)) {
                let real_str = real.to_string_lossy().to_string();
                if real_str != jh {
                    log::info!("JAVA_HOME canonicalize: {} -> {}", jh, real_str);
                    std::env::set_var("JAVA_HOME", real_str);
                }
            }
        }
    } else {
        // JAVA_HOME 未设置，从注册表补设
        if let Some(jh) = read_reg(HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment", "JAVA_HOME")
            .or_else(|| read_reg(HKEY_CURRENT_USER, r"Environment", "JAVA_HOME"))
        {
            if !jh.is_empty() {
                let real = crate::util::canonicalize_clean(std::path::Path::new(&jh))
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or(jh.clone());
                log::info!("从注册表补设 JAVA_HOME = {} (raw: {})", real, jh);
                std::env::set_var("JAVA_HOME", real);
            }
        }
    }
    if std::env::var("MAVEN_HOME").ok().filter(|s| !s.is_empty()).is_none() {
        if let Some(mh) = read_reg(HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment", "MAVEN_HOME")
            .or_else(|| read_reg(HKEY_CURRENT_USER, r"Environment", "MAVEN_HOME"))
        {
            if !mh.is_empty() {
                log::info!("从注册表补设 MAVEN_HOME = {}", mh);
                std::env::set_var("MAVEN_HOME", mh);
            }
        }
    }
}

#[cfg(not(windows))]
pub fn merge_registry_path() {}
