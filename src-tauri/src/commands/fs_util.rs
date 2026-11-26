//! 文件系统工具（打包产物打开目录）。
use crate::error::{AppError, AppResult};

// ============================ Util（文件系统） ============================

/// 在系统文件管理器中定位显示指定绝对路径（打包产物 / target 目录）
#[tauri::command]
pub async fn reveal_path_in_file_manager(path: String) -> AppResult<()> {
    tokio::task::spawn_blocking(move || {
        crate::project_fs::reveal_path(&path)
    })
    .await
    .map_err(|e| AppError::Other(format!("打开文件管理器任务失败: {}", e)))?
}

