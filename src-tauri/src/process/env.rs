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

/// 环境配置（从项目解析得出）
#[derive(Clone)]
pub struct EnvConfig {
    pub java_home: Option<String>,
    pub maven_home: Option<String>,
    /// 项目根路径（用于多模块 install）
    pub project_root: Option<String>,
}

/// 从服务的 project_id 查项目，解析出项目级 JDK / Maven 配置
pub fn resolve_env_config(service: &Service) -> AppResult<EnvConfig> {
    let mut cfg = EnvConfig {
        java_home: None,
        maven_home: None,
        project_root: None,
    };
    if let Some(pid) = &service.project_id {
        if let Ok(project) = db::get_project(pid) {
            cfg.java_home = project.java_home.and_then(non_empty);
            cfg.maven_home = project.maven_home.and_then(non_empty);
            cfg.project_root = Some(project.root_path);
        }
    }
    Ok(cfg)
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

/// 确定生效的 JAVA_HOME：项目配置优先，否则用系统环境变量。
/// 返回 canonicalize 后的真实路径，避免 scoop current junction 在 elevated 进程中无法解析。
pub fn resolve_java_home(cfg: &EnvConfig) -> Option<String> {
    let raw = cfg
        .java_home
        .clone()
        .or_else(|| std::env::var("JAVA_HOME").ok().filter(|s| !s.is_empty()))?;
    // canonicalize 解析 junction，失败时返回原路径
    Some(
        crate::util::canonicalize_clean(std::path::Path::new(&raw))
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or(raw),
    )
}

/// 启动前预检：确认 java / mvn 可用
pub fn preflight_check(
    cfg: &EnvConfig,
    working_dir: &Path,
    program: &str,
) -> AppResult<()> {
    // 1. java 可执行性检查
    let java_home = resolve_java_home(cfg);
    let java_bin = if let Some(jh) = &java_home {
        PathBuf::from(jh).join("bin").join("java.exe")
    } else {
        PathBuf::from("java.exe")
    };
    let java_ok = java_home.is_some() && crate::util::path_exists_follow_junction(&java_bin);
    if !java_ok && which_java().is_none() {
        return Err(crate::error::AppError::Process(format!(
            "未找到可用的 JDK。\n{}请在该服务所属项目的设置里指定 JDK 路径，或确保系统 JAVA_HOME / PATH 配置正确。",
            if java_home.is_some() {
                format!("配置的 JAVA_HOME 不存在: {}\n", java_home.unwrap())
            } else {
                "未设置 JAVA_HOME。\n".to_string()
            }
        )));
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

/// 为子进程注入环境变量：只覆盖 JAVA_HOME / MAVEN_HOME / PATH，其余走系统继承
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
