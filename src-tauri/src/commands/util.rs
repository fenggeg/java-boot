//! 浏览器打开端口。
use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;
use crate::error::AppResult;

/// 打开浏览器端口
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
