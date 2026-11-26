//! 文件系统工具：在系统文件管理器中定位显示指定路径。
//!
//! 供打包产物「打开目录」等服务卡片功能使用，与项目文件浏览/编辑无关。

use std::path::PathBuf;

use crate::error::{AppError, AppResult};

/// 在系统文件管理器中定位显示指定绝对路径（不依赖项目根目录）
pub fn reveal_path(path: &str) -> AppResult<()> {
    let full = PathBuf::from(path);
    if !full.exists() {
        return Err(AppError::NotFound(format!("文件不存在: {}", path)));
    }
    #[cfg(windows)]
    let target = crate::util::canonicalize_clean(&full).unwrap_or(full);
    #[cfg(not(windows))]
    let target = full;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", target.display()))
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|e| AppError::Other(format!("打开文件管理器失败: {}", e)))?;
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("xdg-open")
            .arg(target.parent().unwrap_or(&target))
            .spawn()
            .map_err(|e| AppError::Other(format!("打开文件管理器失败: {}", e)))?;
    }
    Ok(())
}
