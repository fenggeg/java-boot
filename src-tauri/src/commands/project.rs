//! 项目：扫描 / 添加 / 删除 / 重新扫描 / 项目环境配置。
use std::path::PathBuf;
use tauri::AppHandle;
use crate::db;
use crate::db::models::{Project, ScannedModule, Service};
use crate::error::AppResult;
use crate::pom;
use crate::process;
use crate::watcher;
use super::tools::{detect_jdks, detect_mavens, JdkInfo};

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

