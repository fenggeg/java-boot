//! 环境解析 & 命令定位
//!
//! 从原 manager.rs 抽离：
//! - `EnvConfig` / `resolve_env_config` / `resolve_maven_cmd` / `resolve_java_home`
//! - `preflight_check` / `which_java` / `which_mvn`（用 `OnceLock` 缓存 PATH 探测）
//! - `inject_env`：给子进程注入 JAVA_HOME / MAVEN_HOME / PATH
//!   （去掉了 `env_clear + 复制全部环境变量` 的反模式，直接 override 需要的 key）

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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
        if mvn_cmd.exists() {
            return (
                "cmd".to_string(),
                vec!["/c".to_string(), mvn_cmd.to_string_lossy().to_string()],
            );
        }
        let mvn_bin = PathBuf::from(mh).join("bin").join("mvn");
        if mvn_bin.exists() {
            return (mvn_bin.to_string_lossy().to_string(), vec![]);
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

/// 确定生效的 JAVA_HOME：项目配置优先，否则用系统环境变量
pub fn resolve_java_home(cfg: &EnvConfig) -> Option<String> {
    cfg.java_home
        .clone()
        .or_else(|| std::env::var("JAVA_HOME").ok().filter(|s| !s.is_empty()))
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
    let java_ok = java_home.is_some() && java_bin.exists();
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

/// 在 PATH 中查找 java（首次调用时扫描 PATH，之后走缓存）
pub fn which_java() -> Option<String> {
    static CACHE: OnceLock<Option<String>> = OnceLock::new();
    CACHE.get_or_init(scan_java).clone()
}

fn scan_java() -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("java.exe");
        if candidate.exists() {
            return Some(candidate.to_string_lossy().to_string());
        }
    }
    None
}

/// 在 PATH 中查找 mvn（缓存）
pub fn which_mvn() -> Option<String> {
    static CACHE: OnceLock<Option<String>> = OnceLock::new();
    CACHE.get_or_init(scan_mvn).clone()
}

fn scan_mvn() -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("mvn.cmd");
        if candidate.exists() {
            return Some(candidate.to_string_lossy().to_string());
        }
        let candidate = dir.join("mvn");
        if candidate.exists() {
            return Some(candidate.to_string_lossy().to_string());
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
