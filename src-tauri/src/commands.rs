use std::path::PathBuf;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use tokio::sync::Mutex;
use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;

use crate::db;
use crate::db::models::{AppConfig, Project, ScannedModule, Service, ServiceRuntime};
use crate::error::{AppError, AppResult};
use crate::util::CandidateCollector;
use crate::util::NoWindow;
use crate::pom;
use crate::process;
use crate::watcher;

/// JDK/Maven 探测结果缓存：配置弹窗反复打开/切换项目时，避免每次并发启动
/// 多个 JVM（`java -version` / `mvn -v`）造成数秒卡顿。60 秒内直接复用结果。
const TOOL_CACHE_TTL: Duration = Duration::from_secs(60);

static JDK_CACHE: Lazy<Mutex<Option<(Instant, Vec<JdkInfo>)>>> =
    Lazy::new(|| Mutex::new(None));
static MAVEN_CACHE: Lazy<Mutex<Option<(Instant, Vec<MavenInfo>)>>> =
    Lazy::new(|| Mutex::new(None));

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
/// 若项目尚未配置 JDK / Maven，自动从 pom 声明的 Java 版本匹配已安装的 JDK，并选取 Maven 写入项目配置
#[tauri::command]
pub async fn add_project(path: String, selected_modules: Vec<ScannedModule>) -> AppResult<Project> {
    let project_root = PathBuf::from(&path);

    // 复用已存在的项目（同 root_path）
    let mut project = match db::find_project_by_path(&project_root.to_string_lossy())? {
        Some(p) => p,
        None => {
            let name = project_root
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "未命名项目".to_string());
            db::insert_project(&name, &project_root.to_string_lossy())?
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

    // ===== 自动配置 JDK / Maven（仅当项目尚未配置时）=====
    let mut jdk_home: Option<String> = None;
    let mut maven_home: Option<String> = None;

    if project.java_home.is_none() {
        // 取选中的服务里第一个声明的 Java 版本（同项目通常统一）
        let required = selected_modules
            .iter()
            .filter(|m| m.is_service)
            .find_map(|m| m.java_version.clone());
        let jdks = detect_jdks().await;
        if let Some(sel) = pick_jdk(required.as_deref(), &jdks) {
            if crate::util::path_exists_follow_junction(
                &PathBuf::from(&sel.path).join("bin").join("java.exe"),
            ) {
                jdk_home = Some(sel.path.clone());
            }
        }
    }

    if project.maven_home.is_none() {
        // 项目自带 mvnw 时跳过：运行时优先使用 mvnw，无需写死系统 Maven
        let has_mvnw = ["mvnw.cmd", "mvnw.bat", "mvnw"]
            .iter()
            .any(|f| project_root.join(f).exists());
        if !has_mvnw {
            let mavens = detect_mavens().await;
            if let Some(m) = mavens.first() {
                if crate::util::path_exists_follow_junction(
                    &PathBuf::from(&m.path).join("bin").join("mvn.cmd"),
                ) {
                    maven_home = Some(m.path.clone());
                }
            }
        }
    }

    if jdk_home.is_some() || maven_home.is_some() {
        db::update_project_env(
            &project.id,
            jdk_home.as_deref().map(Some),
            maven_home.as_deref().map(Some),
            None,
        )?;
        if let Some(j) = jdk_home {
            project.java_home = Some(j);
        }
        if let Some(m) = maven_home {
            project.maven_home = Some(m);
        }
    }

    Ok(project)
}

/// 解析 JDK 版本字符串为主版本号："1.8.0_412"→8、"17.0.12"→17、"17"→17
fn jdk_major(version: &str) -> Option<u32> {
    let v = version.trim();
    if v.is_empty() {
        return None;
    }
    let major = v.split(['.', '_', '-', ' ']).next()?.parse::<u32>().ok()?;
    if major == 1 {
        // Java 8 及以前："1.8.x" → 8
        v.split('.').nth(1)?.parse::<u32>().ok()
    } else {
        Some(major)
    }
}

/// 根据项目要求的 Java 版本，从已探测的 JDK 列表中选择最合适的：
/// 精确匹配主版本优先；否则取主版本差值最小（并列取较高版本）；未声明版本取第一个
fn pick_jdk(required: Option<&str>, jdks: &[JdkInfo]) -> Option<JdkInfo> {
    if jdks.is_empty() {
        return None;
    }
    let target = required.and_then(jdk_major);
    match target {
        None => Some(jdks[0].clone()),
        Some(t) => {
            if let Some(hit) = jdks.iter().find(|j| jdk_major(&j.version) == Some(t)) {
                return Some(hit.clone());
            }
            let mut best: Option<(&JdkInfo, i64, u32)> = None;
            for j in jdks {
                let Some(m) = jdk_major(&j.version) else {
                    continue;
                };
                let diff = (m as i64 - t as i64).abs();
                let better = match best {
                    None => true,
                    Some((_, bd, bm)) => diff < bd || (diff == bd && m > bm),
                };
                if better {
                    best = Some((j, diff, m));
                }
            }
            best.map(|(j, _, _)| j.clone())
        }
    }
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
    // 先停止该项目下所有运行中的服务
    let services = db::list_services_by_project(&project_id)?;
    for s in &services {
        // 覆盖 Running/Starting/Recompiling/Stopping 等所有活跃状态
        if process::get_manager().is_running(&s.id) {
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

    // 手动添加的服务不自动归属任何项目
    let project_id: Option<String> = None;

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
    env_vars: Option<Option<String>>,
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
        env_vars.as_ref().map(|o| o.as_deref()),
    )
}

/// 更新项目级 JDK / Maven / 环境变量配置
#[tauri::command]
pub fn update_project_env(
    project_id: String,
    java_home: Option<Option<String>>,
    maven_home: Option<Option<String>>,
    env_vars: Option<Option<String>>,
) -> AppResult<()> {
    // 路径规范化：去除首尾空白，统一正斜杠为反斜杠（Windows），去除多余分隔符
    let normalize = |s: Option<Option<String>>| -> Option<Option<String>> {
        s.map(|inner| {
            inner.map(|v| {
                let trimmed = v.trim();
                if trimmed.is_empty() {
                    String::new()
                } else {
                    // 统一斜杠方向，合并连续反斜杠（但保留 UNC 前缀 \\）
                    let mut result = String::with_capacity(trimmed.len());
                    let mut prev_bs = false;
                    for (i, c) in trimmed.chars().enumerate() {
                        if c == '/' {
                            result.push('\\');
                            prev_bs = true;
                        } else if c == '\\' {
                            // 保留 UNC 路径开头的双反斜杠
                            if i < 2 && trimmed.starts_with(r"\\") {
                                result.push('\\');
                            } else if !prev_bs {
                                result.push('\\');
                            }
                            prev_bs = true;
                        } else {
                            result.push(c);
                            prev_bs = false;
                        }
                    }
                    result
                }
            })
        })
    };
    db::update_project_env(
        &project_id,
        normalize(java_home).as_ref().map(|o| o.as_deref()),
        normalize(maven_home).as_ref().map(|o| o.as_deref()),
        env_vars.as_ref().map(|o| o.as_deref()),
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
    db::update_service(&id, None, Some(enabled), None, None, None, None, None, None)?;
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
pub async fn recompile_and_start(id: String, app: AppHandle) -> AppResult<()> {
    let service = db::get_service(&id)?;
    process::get_manager().recompile_and_start(app, service).await
}

/// 清理服务编译产物（mvn clean），不重新启动
#[tauri::command]
pub async fn clean_service(id: String, app: AppHandle) -> AppResult<()> {
    let service = db::get_service(&id)?;
    process::get_manager().clean_service(app, service).await
}

#[tauri::command]
pub async fn stop_all(app: AppHandle) -> AppResult<()> {
    process::get_manager().stop_all(app).await
}

/// 带依赖启动：按拓扑序先启动依赖，再启动目标服务
#[tauri::command]
pub async fn start_service_with_dependencies(id: String, app: AppHandle) -> AppResult<()> {
    let service = db::get_service(&id)?;
    process::get_manager().start_with_dependencies(app, service).await
}

/// 批量启动多个服务（一键启动项目下所有服务，含依赖编排）
#[tauri::command]
pub async fn start_services_batch(
    ids: Vec<String>,
    app: AppHandle,
) -> AppResult<process::manager::BatchStartResult> {
    process::get_manager().start_services_batch(app, &ids).await
}

/// 查询服务的直接依赖列表
#[tauri::command]
pub fn get_service_dependencies(id: String) -> AppResult<Vec<String>> {
    db::list_dependencies(&id)
}

/// 设置服务的依赖列表（全量替换）
#[tauri::command]
pub fn set_service_dependencies(id: String, depends_on_ids: Vec<String>) -> AppResult<()> {
    db::set_dependencies(&id, &depends_on_ids)
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

// ============================ Files（项目文件浏览/编辑） ============================

/// 列出项目根下某目录（单层，惰性加载用）
#[tauri::command]
pub async fn list_files(project_id: String, path: String) -> AppResult<Vec<crate::project_fs::FileEntry>> {
    tokio::task::spawn_blocking(move || crate::project_fs::list_dir(&project_id, &path))
        .await
        .map_err(|e| AppError::Other(format!("列目录任务失败: {}", e)))?
}

/// 读取项目文件（UTF-8/GBK 探测，非 UTF-8 只读）
#[tauri::command]
pub async fn read_project_file(
    project_id: String,
    path: String,
) -> AppResult<crate::project_fs::FileContent> {
    tokio::task::spawn_blocking(move || crate::project_fs::read_file(&project_id, &path))
        .await
        .map_err(|e| AppError::Other(format!("读取文件任务失败: {}", e)))?
}

/// 写回项目文件（UTF-8）
#[tauri::command]
pub async fn write_project_file(
    project_id: String,
    path: String,
    content: String,
) -> AppResult<()> {
    tokio::task::spawn_blocking(move || crate::project_fs::write_file(&project_id, &path, &content))
        .await
        .map_err(|e| AppError::Other(format!("写入文件任务失败: {}", e)))?
}

/// 获取文件绝对路径（前端图片预览用）
#[tauri::command]
pub async fn get_file_abs_path(
    project_id: String,
    path: String,
) -> AppResult<String> {
    tokio::task::spawn_blocking(move || crate::project_fs::get_file_abs_path(&project_id, &path))
        .await
        .map_err(|e| AppError::Other(format!("获取路径失败: {}", e)))?
}

/// 重命名项目内文件 / 目录，返回新相对路径
#[tauri::command]
pub async fn fs_rename(
    project_id: String,
    path: String,
    new_name: String,
) -> AppResult<String> {
    tokio::task::spawn_blocking(move || {
        crate::project_fs::rename_entry(&project_id, &path, &new_name)
    })
    .await
    .map_err(|e| AppError::Other(format!("重命名任务失败: {}", e)))?
}

/// 复制文件 / 目录到目标目录，返回新相对路径
#[tauri::command]
pub async fn fs_copy_entry(
    project_id: String,
    src_path: String,
    dest_dir: String,
) -> AppResult<String> {
    tokio::task::spawn_blocking(move || {
        crate::project_fs::copy_entry(&project_id, &src_path, &dest_dir)
    })
    .await
    .map_err(|e| AppError::Other(format!("复制任务失败: {}", e)))?
}

/// 移动文件 / 目录到目标目录，返回新相对路径
#[tauri::command]
pub async fn fs_move_entry(
    project_id: String,
    src_path: String,
    dest_dir: String,
) -> AppResult<String> {
    tokio::task::spawn_blocking(move || {
        crate::project_fs::move_entry(&project_id, &src_path, &dest_dir)
    })
    .await
    .map_err(|e| AppError::Other(format!("移动任务失败: {}", e)))?
}

/// 在系统文件管理器中定位显示该条目
#[tauri::command]
pub async fn reveal_in_file_manager(project_id: String, path: String) -> AppResult<()> {
    tokio::task::spawn_blocking(move || {
        crate::project_fs::reveal_in_file_manager(&project_id, &path)
    })
    .await
    .map_err(|e| AppError::Other(format!("打开文件管理器任务失败: {}", e)))?
}

/// 扁平遍历项目内全部文件（排除依赖/构建目录与符号链接），
/// 供前端做文件名快速搜索（Ctrl+P 快速打开）
#[tauri::command]
pub async fn walk_files(project_id: String) -> AppResult<Vec<crate::project_fs::FlatFile>> {
    tokio::task::spawn_blocking(move || crate::project_fs::walk_files(&project_id))
        .await
        .map_err(|e| AppError::Other(format!("遍历项目文件任务失败: {}", e)))?
}

// ============================ Terminal（集成终端） ============================

/// 为项目创建交互式终端会话，返回会话 id
#[tauri::command]
pub async fn terminal_create(project_id: String, app: AppHandle) -> AppResult<String> {
    crate::terminal::create(app, &project_id).await
}

/// 向终端会话写入输入数据（xterm.js 键盘原始数据透传）
#[tauri::command]
pub async fn terminal_write(session_id: String, data: String) -> AppResult<()> {
    crate::terminal::write(&session_id, &data).await
}

/// 调整伪终端尺寸（前端 xterm.js fit 后调用）
#[tauri::command]
pub async fn terminal_resize(
    session_id: String,
    cols: u16,
    rows: u16,
) -> AppResult<()> {
    crate::terminal::resize(&session_id, cols, rows).await
}

/// 终止终端会话
#[tauri::command]
pub async fn terminal_kill(session_id: String) -> AppResult<()> {
    crate::terminal::kill(&session_id).await
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
    // 持锁检查缓存：命中则直接返回，未命中则持锁执行探测，
    // 确保并发调用不会重复启动 JVM 探测进程
    let mut _guard = JDK_CACHE.lock().await;
    if let Some((ts, cached)) = _guard.as_ref() {
        if ts.elapsed() < TOOL_CACHE_TTL {
            return cached.clone();
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
    let cloned = result.clone();
    *_guard = Some((Instant::now(), cloned));
    result
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
    // 持锁检查缓存：命中则直接返回，未命中则持锁执行探测，
    // 确保并发调用不会重复启动 Maven 探测进程
    let mut _guard = MAVEN_CACHE.lock().await;
    if let Some((ts, cached)) = _guard.as_ref() {
        if ts.elapsed() < TOOL_CACHE_TTL {
            return cached.clone();
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
    // 先检测 JDK，把第一个可用 JAVA_HOME 传给 maven 探测
    // （安装器启动时 PATH 不完整，probe_maven 内部 which_java() 可能失败）
    let jdk_candidates = tokio::task::spawn_blocking(collect_jdk_candidates)
        .await
        .unwrap_or_default();
    log::info!("detect_mavens: jdk 候选 {} 个: {:?}", jdk_candidates.len(), jdk_candidates);
    let mut jdks = detect_tools(jdk_candidates, probe_jdk).await;
    // 去重：current junction 和真实目录可能产生重复
    let mut seen_jdk: std::collections::HashSet<String> = std::collections::HashSet::new();
    jdks.retain(|j| seen_jdk.insert(j.path.to_lowercase()));
    log::info!("detect_mavens: 探测到 {} 个 JDK（去重后）: {:?}", jdks.len(), jdks.iter().map(|j| &j.path).collect::<Vec<_>>());
    let fallback_java_home = jdks.first().map(|j| j.path.clone());
    let mut result = detect_tools(candidates, move |m| probe_maven(m, fallback_java_home.as_deref())).await;
    let mut seen_mvn: std::collections::HashSet<String> = std::collections::HashSet::new();
    result.retain(|m| seen_mvn.insert(m.path.to_lowercase()));
    log::info!("detect_mavens: 探测到 {} 个 maven（去重后）: {:?}", result.len(), result.iter().map(|m| (&m.path, &m.version)).collect::<Vec<_>>());
    let cloned = result.clone();
    *_guard = Some((Instant::now(), cloned));
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
