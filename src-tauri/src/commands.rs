use std::path::PathBuf;

use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;

use crate::db;
use crate::db::models::{AppConfig, Project, ScannedModule, Service, ServiceRuntime};
use crate::error::AppResult;
use crate::util::CandidateCollector;
use crate::git;
use crate::pom;
use crate::process;
use crate::watcher;

// ============================ Project ============================

#[tauri::command]
pub fn list_projects() -> AppResult<Vec<Project>> {
    db::list_projects()
}

#[tauri::command]
pub fn list_services() -> AppResult<Vec<Service>> {
    db::list_services()
}

/// 扫描项目目录，返回 module 树（添加项目对话框用）
#[tauri::command]
pub fn scan_project(path: String) -> AppResult<Vec<ScannedModule>> {
    let root = PathBuf::from(&path);
    let root_pom = root.join("pom.xml");
    if !root_pom.exists() {
        return Err(crate::error::AppError::NotFound(format!(
            "未找到 pom.xml: {}",
            root_pom.display()
        )));
    }
    pom::scan_project(&root_pom)
}

/// 添加项目：根据扫描结果勾选的 module 列表批量添加服务
#[tauri::command]
pub fn add_project(path: String, selected_modules: Vec<ScannedModule>) -> AppResult<Project> {
    let root = PathBuf::from(&path);
    let git_root = pom::find_git_root(&root);
    let project_root = git_root.clone().unwrap_or_else(|| root.clone());
    let git_available = git_root.is_some();

    // 复用已存在的项目（同 root_path）
    let project = match db::find_project_by_path(&project_root.to_string_lossy())? {
        Some(p) => p,
        None => {
            let name = project_root
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "未命名项目".to_string());
            db::insert_project(&name, &project_root.to_string_lossy(), git_available)?
        }
    };

    for module in &selected_modules {
        if !module.is_service {
            continue;
        }
        if module.already_added {
            continue;
        }
        let pom_path = PathBuf::from(&module.pom_path);
        let working_dir = pom_path
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        db::insert_service(
            &module.artifact_id,
            &module.pom_path,
            &working_dir,
            Some(&project.id),
            module.main_class.as_deref(),
        )?;
    }

    Ok(project)
}

/// 重新扫描项目并补充添加新 module
#[tauri::command]
pub fn rescan_project(project_id: String) -> AppResult<Vec<ScannedModule>> {
    let project = db::get_project(&project_id)?;
    let root_pom = PathBuf::from(&project.root_path).join("pom.xml");
    if !root_pom.exists() {
        return Err(crate::error::AppError::NotFound(format!(
            "项目 pom.xml 不存在: {}",
            root_pom.display()
        )));
    }
    pom::scan_project(&root_pom)
}

#[tauri::command]
pub async fn delete_project(project_id: String, app: AppHandle) -> AppResult<()> {
    // 先停止该项目下所有运行中/拉取中的服务
    let services = db::list_services_by_project(&project_id)?;
    for s in &services {
        let rt = process::get_manager().get_runtime(&s.id);
        // 覆盖 Running/Starting/Recompiling/Pulling/Stopping 等所有活跃状态
        if process::get_manager().is_running(&s.id)
            || matches!(rt.status, crate::db::models::ServiceStatus::Pulling)
        {
            process::get_manager()
                .stop(app.clone(), &s.id)
                .await?;
        }
        watcher::get_watch_manager().unwatch(&s.id);
    }
    db::delete_project(&project_id)?;
    Ok(())
}

// ============================ Service ============================

/// 添加单个服务（手动指定 pom.xml）
#[tauri::command]
pub fn add_service(pom_path: String, name: Option<String>) -> AppResult<Service> {
    let p = PathBuf::from(&pom_path);
    if !p.exists() {
        return Err(crate::error::AppError::NotFound(format!(
            "pom.xml 不存在: {}",
            pom_path
        )));
    }
    let info = pom::parse_pom(&p)?;
    let svc_name = name.unwrap_or_else(|| {
        if info.artifact_id.is_empty() {
            p.parent()
                .and_then(|d| d.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "service".to_string())
        } else {
            info.artifact_id.clone()
        }
    });
    let working_dir = p
        .parent()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();

    // 确定项目归属（parent 为 None 时跳过 git 归属，避免误归属到无关仓库）
    let project_id = if let Some(parent) = p.parent() {
        if let Some(git_root) = pom::find_git_root(parent) {
            match db::find_project_by_path(&git_root.to_string_lossy())? {
                Some(proj) => Some(proj.id),
                None => {
                    let pname = git_root
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "未命名项目".to_string());
                    let proj = db::insert_project(&pname, &git_root.to_string_lossy(), true)?;
                    Some(proj.id)
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    db::insert_service(&svc_name, &pom_path, &working_dir, project_id.as_deref(), None)
}

#[tauri::command]
pub fn update_service(
    id: String,
    name: Option<String>,
    auto_restart: Option<bool>,
    maven_opts: Option<Option<String>>,
    profiles: Option<Option<String>>,
    dev_mode: Option<bool>,
    main_class: Option<Option<String>>,
    override_properties: Option<Option<String>>,
) -> AppResult<()> {
    db::update_service(
        &id,
        name.as_deref(),
        auto_restart,
        maven_opts.as_ref().map(|o| o.as_deref()),
        profiles.as_ref().map(|o| o.as_deref()),
        dev_mode,
        main_class.as_ref().map(|o| o.as_deref()),
        override_properties.as_ref().map(|o| o.as_deref()),
    )
}

/// 更新项目级 JDK / Maven 配置
#[tauri::command]
pub fn update_project_env(
    project_id: String,
    java_home: Option<Option<String>>,
    maven_home: Option<Option<String>>,
) -> AppResult<()> {
    db::update_project_env(
        &project_id,
        java_home.as_ref().map(|o| o.as_deref()),
        maven_home.as_ref().map(|o| o.as_deref()),
    )
}

#[tauri::command]
pub async fn delete_service(id: String, app: AppHandle) -> AppResult<()> {
    process::get_manager().stop(app.clone(), &id).await?;
    watcher::get_watch_manager().unwatch(&id);
    db::delete_service(&id)?;
    Ok(())
}

/// 切换自动重启开关
#[tauri::command]
pub fn toggle_auto_restart(id: String, enabled: bool, app: AppHandle) -> AppResult<()> {
    db::update_service(&id, None, Some(enabled), None, None, None, None, None)?;
    let service = db::get_service(&id)?;
    if enabled {
        let _ = watcher::get_watch_manager().watch(app, service);
    } else {
        watcher::get_watch_manager().unwatch(&id);
    }
    Ok(())
}

// ============================ Process ============================

#[tauri::command]
pub async fn start_service(id: String, app: AppHandle) -> AppResult<()> {
    let service = db::get_service(&id)?;
    process::get_manager().start(app, service).await
}

#[tauri::command]
pub async fn stop_service(id: String, app: AppHandle) -> AppResult<()> {
    process::get_manager().stop(app, &id).await
}

#[tauri::command]
pub async fn restart_service(id: String, app: AppHandle) -> AppResult<()> {
    let service = db::get_service(&id)?;
    process::get_manager().restart(app, service).await
}

#[tauri::command]
pub async fn compile_and_start(id: String, app: AppHandle) -> AppResult<()> {
    let service = db::get_service(&id)?;
    process::get_manager().compile_and_start(app, service).await
}

#[tauri::command]
pub async fn stop_all(app: AppHandle) -> AppResult<()> {
    process::get_manager().stop_all(app).await
}

#[tauri::command]
pub fn get_runtime(id: String) -> ServiceRuntime {
    process::get_manager().get_runtime(&id)
}

#[tauri::command]
pub fn get_all_runtimes() -> Vec<ServiceRuntime> {
    process::get_manager().all_runtimes()
}

#[tauri::command]
pub fn refresh_port_conflicts(app: AppHandle) {
    process::get_manager().refresh_port_conflicts(&app);
}

// ============================ Git ============================

#[tauri::command]
pub fn git_available() -> bool {
    git::git_available()
}

#[tauri::command]
pub async fn git_pull(project_id: String, app: AppHandle) -> AppResult<git::PullResult> {
    git::pull(app, &project_id).await
}

#[tauri::command]
pub async fn git_pull_and_restart(
    project_id: String,
    app: AppHandle,
) -> AppResult<git::PullResult> {
    git::pull_and_restart(app, &project_id).await
}

// ============================ Config ============================

#[tauri::command]
pub fn get_config() -> AppResult<AppConfig> {
    db::load_config()
}

#[tauri::command]
pub fn save_config(config: AppConfig) -> AppResult<()> {
    db::save_config(&config)
}

// ============================ Util ============================

/// 在浏览器打开端口
#[tauri::command]
pub fn open_in_browser(port: u16, app: AppHandle) -> AppResult<()> {
    if port == 0 {
        return Err(crate::error::AppError::Other("无效的端口号".into()));
    }
    let url = format!("http://localhost:{}", port);
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| crate::error::AppError::Other(format!("打开浏览器失败: {}", e)))?;
    Ok(())
}

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
#[tauri::command]
pub async fn detect_jdks() -> Vec<JdkInfo> {
    // 候选路径收集涉及大量同步 fs IO，移入 spawn_blocking 避免阻塞 tokio runtime
    let candidates = tokio::task::spawn_blocking(collect_jdk_candidates)
        .await
        .unwrap_or_default();
    detect_tools(candidates, probe_jdk).await
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
            if java_exe.exists() {
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
                        if p.join("bin").join("java.exe").exists() {
                            cc.push(p.to_string_lossy().to_string());
                        }
                        // scoop 下可能再嵌套一层（如 temurin17-jdk/current）
                        if let Ok(sub_entries) = std::fs::read_dir(&p) {
                            for sub in sub_entries.flatten() {
                                let sp = sub.path();
                                if sp.join("bin").join("java.exe").exists() {
                                    cc.push(sp.to_string_lossy().to_string());
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
                    if p.is_dir() && p.join("bin").join("java.exe").exists() {
                        cc.push(p.to_string_lossy().to_string());
                    }
                }
            }
        }
    }

    cc.into_candidates()
}

#[derive(serde::Serialize)]
pub struct JdkInfo {
    pub path: String,
    pub version: String,
    pub vendor: String,
}

/// 探测某 JDK_HOME 的版本信息
fn probe_jdk(java_home: &str) -> Option<JdkInfo> {
    let java_exe = std::path::PathBuf::from(java_home).join("bin").join("java.exe");
    if !java_exe.exists() {
        return None;
    }
    let output = std::process::Command::new(&java_exe)
        .arg("-version")
        .output()
        .ok()?;
    // java -version 输出到 stderr
    let text = String::from_utf8_lossy(&output.stderr);
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
    Some(JdkInfo {
        path: java_home.to_string(),
        version,
        vendor,
    })
}

#[derive(serde::Serialize)]
pub struct MavenInfo {
    pub path: String,
    pub version: String,
}

/// 探测系统已安装的 Maven 列表
#[tauri::command]
pub async fn detect_mavens() -> Vec<MavenInfo> {
    // 候选路径收集涉及大量同步 fs IO，移入 spawn_blocking 避免阻塞 tokio runtime
    let candidates = tokio::task::spawn_blocking(collect_maven_candidates)
        .await
        .unwrap_or_default();
    detect_tools(candidates, probe_maven).await
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
            if mvn_exe.exists() {
                if let Some(bin_dir) = dir.parent() {
                    cc.push(bin_dir.to_string_lossy().to_string());
                }
            }
        }
    }

    // 3. scoop 安装目录
    if let Ok(home) = std::env::var("USERPROFILE") {
        let scoop_maven = format!("{}\\scoop\\apps\\maven", home);
        if let Ok(entries) = std::fs::read_dir(&scoop_maven) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.join("bin").join("mvn.cmd").exists() {
                    cc.push(p.to_string_lossy().to_string());
                }
            }
        }
        let scoop_current = format!("{}\\scoop\\apps\\maven\\current", home);
        if std::path::Path::new(&scoop_current).join("bin").join("mvn.cmd").exists() {
            cc.push(scoop_current);
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
                if p.is_dir() && p.join("bin").join("mvn.cmd").exists() {
                    cc.push(p.to_string_lossy().to_string());
                }
            }
        }
    }

    cc.into_candidates()
}

/// 探测某 MAVEN_HOME 的版本信息
fn probe_maven(maven_home: &str) -> Option<MavenInfo> {
    let mvn_cmd = std::path::PathBuf::from(maven_home).join("bin").join("mvn.cmd");
    if !mvn_cmd.exists() {
        return None;
    }
    // 用 JAVA_HOME（如有）执行 mvn -v
    let java_home = std::env::var("JAVA_HOME").ok();
    let mut cmd = std::process::Command::new("cmd");
    cmd.arg("/c").arg(&mvn_cmd).arg("-v");
    if let Some(jh) = &java_home {
        cmd.env("JAVA_HOME", jh);
        let cur_path = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{}\\bin;{}", jh, cur_path));
    }
    let output = cmd.output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let version = text.lines().next().unwrap_or("unknown").trim().to_string();
    Some(MavenInfo {
        path: maven_home.to_string(),
        version,
    })
}
