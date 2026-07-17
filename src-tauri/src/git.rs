use std::path::Path;
use std::process::Command;

use tauri::AppHandle;

use crate::db;
use crate::error::{AppError, AppResult};
use crate::process;

/// 拉取结果
#[derive(Debug, Clone, serde::Serialize)]
pub struct PullResult {
    pub project_id: String,
    pub success: bool,
    pub up_to_date: bool,
    pub message: String,
}

pub fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 检测目录是否为 Git 仓库
pub fn is_git_repo(path: &Path) -> bool {
    path.join(".git").exists()
}

/// 执行 git pull
pub async fn pull(app: AppHandle, project_id: &str) -> AppResult<PullResult> {
    let project = db::get_project(project_id)?;
    let root = Path::new(&project.root_path);

    if !is_git_repo(root) {
        return Err(AppError::Git(format!(
            "{} 不是 Git 仓库",
            root.display()
        )));
    }

    // 互斥检查：项目下任一服务正在编译/启动则禁用
    let services = db::list_services_by_project(project_id)?;
    for s in &services {
        let rt = process::get_manager().get_runtime(&s.id);
        if matches!(
            rt.status,
            db::models::ServiceStatus::Starting
                | db::models::ServiceStatus::Recompiling
                | db::models::ServiceStatus::Pulling
        ) {
            return Err(AppError::Git(format!(
                "项目下服务 {} 正在启动/编译中，请稍后重试",
                s.name
            )));
        }
    }

    // 标记项目下所有服务为"拉取中"
    for s in &services {
        process::get_manager().set_status(
            &app,
            &s.id,
            db::models::ServiceStatus::Pulling,
        );
    }

    let root_clone = root.to_path_buf();
    let project_id_owned = project_id.to_string();
    let app_clone = app.clone();
    let services_clone = services.clone();

    // git pull 可能因需要认证而永久阻塞 spawn_blocking 线程，
    // 用 tokio::time::timeout 包裹，60 秒超时
    let pull_timeout = std::time::Duration::from_secs(60);
    let join_result = tokio::time::timeout(
        pull_timeout,
        tokio::task::spawn_blocking(move || {
            // 禁止交互式凭据提示，避免私有仓库卡住
            let mut cmd = Command::new("git");
            cmd.arg("pull")
                .arg("--no-progress")
                .current_dir(&root_clone)
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("GIT_ASKPASS", "")
                .env("SSH_ASKPASS", "");
            cmd.output()
        }),
    )
    .await;

    let result = match join_result {
        Ok(Ok(output)) => output.map_err(|e| AppError::Git(format!("git pull 执行失败: {}", e)))?,
        Ok(Err(e)) => return Err(AppError::Git(format!("git pull 任务失败: {}", e))),
        Err(_) => {
            // 超时：恢复服务状态并返回错误
            for s in &services_clone {
                let rt = process::get_manager().get_runtime(&s.id);
                let new_status = if rt.pid.is_some() {
                    db::models::ServiceStatus::Running
                } else {
                    db::models::ServiceStatus::Stopped
                };
                process::get_manager().set_status(&app_clone, &s.id, new_status);
            }
            return Err(AppError::Git(
                "git pull 超时（60 秒），可能需要认证或网络异常".to_string(),
            ));
        }
    };

    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let stderr = String::from_utf8_lossy(&result.stderr).to_string();
    let success = result.status.success();

    let up_to_date = stdout.contains("Already up to date")
        || stdout.contains("Already up-to-date")
        || stdout.contains("已经是最新")
        || stdout.contains("已是最新")
        || stdout.contains("up to date")
        || stdout.contains("up-to-date");

    // 写入项目下所有服务日志
    for s in &services {
        for line in stdout.lines() {
            process::ProcessManager::emit_log_static(
                &app,
                &s.id,
                "[git]",
                line,
            );
        }
        for line in stderr.lines() {
            process::ProcessManager::emit_log_static(
                &app,
                &s.id,
                "[git]",
                line,
            );
        }
        // 恢复状态
        let rt = process::get_manager().get_runtime(&s.id);
        let new_status = if rt.pid.is_some() {
            db::models::ServiceStatus::Running
        } else {
            db::models::ServiceStatus::Stopped
        };
        process::get_manager().set_status(&app, &s.id, new_status);
    }

    let message = if success {
        if up_to_date {
            "已是最新".to_string()
        } else {
            stdout.trim().to_string()
        }
    } else {
        format!("{}\n{}", stdout.trim(), stderr.trim())
    };

    let res = PullResult {
        project_id: project_id_owned,
        success,
        up_to_date,
        message,
    };
    Ok(res)
}

/// 拉取并重启项目下运行中的服务
pub async fn pull_and_restart(app: AppHandle, project_id: &str) -> AppResult<PullResult> {
    let result = pull(app.clone(), project_id).await?;
    if result.success {
        let services = db::list_services_by_project(project_id)?;
        let mgr = process::get_manager();
        for s in services {
            // 仅重启正在运行的服务
            if mgr.is_running(&s.id) {
                if let Err(e) = mgr.compile_and_start(app.clone(), s).await {
                    log::error!("拉取后重启失败: {}", e);
                }
            }
        }
    }
    Ok(result)
}
