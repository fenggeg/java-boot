//! 服务：增删改查 / 自动重启开关。
use std::path::PathBuf;
use tauri::AppHandle;
use crate::db;
use crate::db::models::Service;
use crate::error::AppResult;
use crate::pom;
use crate::process;
use crate::watcher;

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

