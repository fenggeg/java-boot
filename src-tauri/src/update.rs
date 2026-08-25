//! 应用自更新：安装包下载 + 静默安装
//!
//! 流程：
//! 1. 前端从更新接口拿到 download_url（GitHub Releases 安装包直链）
//! 2. [`download_update`] 后端流式下载到本地更新目录，
//!    进度经 `update://progress` 事件上报（前端 listen 渲染进度条）
//! 3. [`install_update`] 以 `/S /R` 启动 NSIS 安装器：
//!    `/S` 静默安装（运行中的实例会被安装器自动结束），
//!    `/R` 安装完成后自动重启应用，当前进程随后退出

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use tauri::{AppHandle, Emitter};

use crate::error::{AppError, AppResult};
use crate::util::NoWindow;

/// 下载进度事件名
const PROGRESS_EVENT: &str = "update://progress";

/// 更新包存放目录：%LOCALAPPDATA%\javaboot-launcher\updates
fn update_dir() -> PathBuf {
    let base = dirs::data_local_dir()
        .unwrap_or_else(|| std::env::temp_dir())
        .join("javaboot-launcher")
        .join("updates");
    let _ = std::fs::create_dir_all(&base);
    base
}

/// 从 URL 提取安全的文件名（仅保留字母数字与 . _ -）
fn file_name_from_url(url: &str) -> Option<String> {
    let raw = url.split(['?', '#']).next()?;
    let name = raw.rsplit(['/', '\\']).next()?.trim();
    let sanitized: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '_' || *c == '-')
        .collect();
    if sanitized.is_empty() {
        return None;
    }
    Some(sanitized)
}

#[derive(Clone, serde::Serialize)]
struct UpdateProgress {
    /// 百分比 0-100（total 未知时保持 0，由前端按 downloaded 展示）
    percent: u32,
}

/// 清理更新目录中的历史安装包（上次升级遗留），失败忽略
fn clean_stale_installers(keep: &std::path::Path) {
    if let Ok(entries) = std::fs::read_dir(update_dir()) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p != keep && p.is_file() {
                let _ = std::fs::remove_file(p);
            }
        }
    }
}

/// 流式下载安装包，返回落盘路径
///
/// 进度经 `update://progress` 事件推送 `{ percent: 0-100 }`，
/// 按整数百分比变化节流，避免事件风暴。
#[tauri::command]
pub async fn download_update(app: AppHandle, url: String) -> AppResult<String> {
    if url.is_empty() {
        return Err(AppError::Other("下载地址为空".to_string()));
    }

    // rustls TLS：复用 http 插件同款栈；连接超时防挂死，读超时不设（大文件慢速链路）
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| AppError::Process(format!("创建 HTTP 客户端失败: {}", e)))?;

    let res = client
        .get(&url)
        .header(reqwest::header::ACCEPT, "application/octet-stream")
        .send()
        .await
        .map_err(|e| AppError::Process(format!("下载请求失败: {}", e)))?;
    if !res.status().is_success() {
        return Err(AppError::Process(format!(
            "下载服务器返回 {}",
            res.status().as_u16()
        )));
    }

    let total = res.content_length().unwrap_or(0);

    let file_name = file_name_from_url(&url)
        .unwrap_or_else(|| format!("javaboot-setup-{}.exe", chrono::Local::now().format("%Y%m%d%H%M%S")));
    let mut target = update_dir().join(file_name);
    if target.extension().is_none() {
        target.set_extension("exe");
    }
    clean_stale_installers(&target);

    let file = std::fs::File::create(&target)
        .map_err(|e| AppError::Io(std::io::Error::new(e.kind(), format!("创建文件失败: {}", e))))?;
    let mut writer = std::io::BufWriter::with_capacity(1024 * 1024, file);

    let mut stream = res.bytes_stream();
    let mut downloaded: u64 = 0;
    let mut last_percent: i64 = -1;

    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|e| AppError::Process(format!("下载数据流中断: {}", e)))?;
        writer.write_all(&chunk).map_err(AppError::Io)?;
        downloaded += chunk.len() as u64;
        if total > 0 {
            let percent = ((downloaded as f64 / total as f64) * 100.0).round() as i64;
            let percent = percent.clamp(0, 100);
            if percent != last_percent {
                last_percent = percent;
                let _ = app.emit(PROGRESS_EVENT, UpdateProgress { percent: percent as u32 });
            }
        }
    }
    writer.flush().map_err(AppError::Io)?;

    log::info!("更新包下载完成: {} ({} bytes)", target.display(), downloaded);
    Ok(target.to_string_lossy().to_string())
}

/// 启动 NSIS 安装器完成升级
///
/// - `/S`：静默模式（若当前实例仍存活，安装器会自动结束它）
/// - `/R`：安装完成后自动重启应用
///
/// 安装器拉起后短暂等待再退出当前进程，确保子进程稳定启动；
/// 安装器不加入 Job Object、无 kill-on-close，本进程退出不影响其继续安装。
#[tauri::command]
pub async fn install_update(app: AppHandle, path: String) -> AppResult<()> {
    let installer = PathBuf::from(&path);
    if !crate::util::path_exists_follow_junction(&installer) {
        return Err(AppError::NotFound(format!("安装包不存在: {}", path)));
    }

    std::process::Command::new(&installer)
        .arg("/S")
        .arg("/R")
        .creation_flags_no_window()
        .spawn()
        .map_err(|e| AppError::Process(format!("启动安装器失败: {}", e)))?;

    log::info!("安装器已启动: {}，即将退出当前应用", installer.display());
    tokio::time::sleep(Duration::from_millis(800)).await;
    app.exit(0);
    Ok(())
}
