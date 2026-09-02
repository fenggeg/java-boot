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
use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Emitter, State};
use tokio_util::sync::CancellationToken;

use crate::error::{AppError, AppResult};
use crate::util::NoWindow;

/// 下载进度事件名
const PROGRESS_EVENT: &str = "update://progress";

/// 允许的下载域名白名单（GitHub Releases 直链 + CDN + 代理）
const ALLOWED_DOWNLOAD_HOSTS: &[&str] = &[
    "github.com",
    "objects.githubusercontent.com",
    "node-red.gyfwork.cc.cd",
];

/// 检查 URL host 是否在白名单中
fn is_allowed_download_host(url: &str) -> bool {
    let parsed = match url::Url::parse(url) {
        Ok(u) => u,
        Err(_) => return false,
    };
    // 仅允许 HTTPS
    if parsed.scheme() != "https" {
        return false;
    }
    let host = parsed.host_str().unwrap_or("");
    ALLOWED_DOWNLOAD_HOSTS.iter().any(|h| host == *h)
}

/// 安全获取 Mutex guard，poison 时恢复而非 panic
fn safe_lock(mutex: &Mutex<Option<CancellationToken>>) -> std::sync::MutexGuard<'_, Option<CancellationToken>> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 进度上报与速度采样的固定间隔：
/// 固定时间窗保证 speed = 窗口内实际字节数 ÷ 实际耗时，与真实吞吐一致
const SAMPLE_INTERVAL: Duration = Duration::from_millis(250);

/// 当前下载任务的取消令牌（同一时刻只允许一个下载）
///
/// `download_update` 启动时新建令牌并存入；`cancel_update` 触发取消；
/// 下载结束（完成/失败/取消）清空。用 Mutex 包裹保证线程安全。
#[derive(Default)]
pub struct DownloadCancel(Mutex<Option<CancellationToken>>);

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
    /// 已下载字节数
    downloaded: u64,
    /// 总字节数（未知为 0）
    total: u64,
    /// 实时下载速度（字节/秒，EMA 平滑）
    speed: u64,
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
/// 进度经 `update://progress` 事件推送 `{ percent, downloaded, total, speed }`，
/// speed 为固定 250ms 窗口实测（Δ字节 ÷ Δ耗时）并轻度 EMA 平滑的速度（字节/秒）。
/// 按固定间隔节流上报，避免高频事件刷爆前端；空闲超时也会采样，停滞时速度能回落到 0。
///
/// 下载过程中可通过 `cancel_update` 命令取消：取消后删除半成品文件并返回错误。
#[tauri::command]
pub async fn download_update(
    app: AppHandle,
    cancel_state: State<'_, DownloadCancel>,
    url: String,
) -> AppResult<String> {
    if url.is_empty() {
        return Err(AppError::Other("下载地址为空".to_string()));
    }
    // URL 白名单校验：阻止非可信来源的下载
    if !is_allowed_download_host(&url) {
        return Err(AppError::Other(format!(
            "下载地址域名不在白名单中: {}",
            url.split('/').nth(2).unwrap_or("未知")
        )));
    }

    // 注册本次下载的取消令牌；若已有旧令牌（异常残留）先取消旧的
    let token = CancellationToken::new();
    {
        let mut guard = safe_lock(&cancel_state.0);
        if let Some(old) = guard.take() {
            old.cancel();
        }
        *guard = Some(token.clone());
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

    // 速度统计：固定 250ms 时间窗采样，瞬时速度 = 窗口内新增字节 ÷ 实际耗时，
    // 再做轻度 EMA 平滑。窗口时长恒定，数值与真实网络吞吐一致，
    // 不会像按百分比事件驱动的可变窗口那样被突发/停顿扭曲。
    let mut stream = res.bytes_stream();
    let mut downloaded: u64 = 0;
    let mut last_tick = std::time::Instant::now();
    let mut last_bytes: u64 = 0;
    let mut speed_ema = 0f64;

    use futures_util::StreamExt;
    loop {
        // 取消检查：cancel_update 触发后立即中止
            if token.is_cancelled() {
                writer.flush().ok();
                drop(writer);
                let _ = std::fs::remove_file(&target);
                {
                    let mut guard = safe_lock(&cancel_state.0);
                    *guard = None;
                }
                return Err(AppError::Other("下载已取消".to_string()));
            }

        // 空闲超时也走一轮采样：停滞时速度能及时回落到 0，而不是停留在旧值
        match tokio::time::timeout(SAMPLE_INTERVAL, stream.next()).await {
            Ok(Some(chunk)) => {
                let chunk = chunk
                    .map_err(|e| AppError::Process(format!("下载数据流中断: {}", e)))?;
                writer.write_all(&chunk).map_err(AppError::Io)?;
                downloaded += chunk.len() as u64;
            }
            Ok(None) => break,
            Err(_) => {}
        }

        let now = std::time::Instant::now();
        let dt = now.duration_since(last_tick).as_secs_f64();
        if dt < SAMPLE_INTERVAL.as_secs_f64() {
            continue;
        }
        let inst = (downloaded - last_bytes) as f64 / dt;
        speed_ema = if speed_ema == 0f64 {
            inst
        } else {
            speed_ema * 0.6 + inst * 0.4
        };
        last_tick = now;
        last_bytes = downloaded;

        let percent = if total > 0 {
            (((downloaded as f64 / total as f64) * 100.0).round() as i64).clamp(0, 100)
        } else {
            -1
        };
        let _ = app.emit(
            PROGRESS_EVENT,
            UpdateProgress {
                percent: percent.max(0) as u32,
                downloaded,
                total,
                speed: speed_ema as u64,
            },
        );
    }
    writer.flush().map_err(AppError::Io)?;

    // 下载完成：清理取消令牌
    {
        let mut guard = safe_lock(&cancel_state.0);
        *guard = None;
    }

    // 收尾：确保前端收到 100% 终态（速度归零）
    if total > 0 {
        let _ = app.emit(
            PROGRESS_EVENT,
            UpdateProgress { percent: 100, downloaded, total, speed: 0 },
        );
    }

    log::info!("更新包下载完成: {} ({} bytes)", target.display(), downloaded);
    Ok(target.to_string_lossy().to_string())
}

/// 取消正在进行的下载
///
/// 触发当前下载任务的取消令牌，`download_update` 循环检测到后
/// 删除半成品文件并返回"下载已取消"错误。无下载任务时为空操作。
#[tauri::command]
pub async fn cancel_update(cancel_state: State<'_, DownloadCancel>) -> AppResult<()> {
    let token = {
        let guard = safe_lock(&cancel_state.0);
        guard.as_ref().cloned()
    };
    if let Some(t) = token {
        t.cancel();
        log::info!("用户取消更新下载");
    }
    Ok(())
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

    // 安全校验：安装包路径必须位于 update_dir() 内，阻止执行任意路径的可执行文件
    let update_dir = update_dir();
    let canonical_installer = crate::util::canonicalize_clean(&installer);
    let canonical_update_dir = crate::util::canonicalize_clean(&update_dir);
    match (&canonical_installer, &canonical_update_dir) {
        (Some(ci), Some(cud)) => {
            if !ci.starts_with(cud) {
                return Err(AppError::Other(
                    "安装包路径不在允许的更新目录中".to_string(),
                ));
            }
        }
        // 若 canonicalize 失败（如路径不存在），用逻辑比较兜底
        _ => {
            let installer_abs = if installer.is_absolute() {
                installer.clone()
            } else {
                std::env::current_dir().unwrap_or_default().join(&installer)
            };
            if !installer_abs.starts_with(&update_dir) {
                return Err(AppError::Other(
                    "安装包路径不在允许的更新目录中".to_string(),
                ));
            }
        }
    }

    if !crate::util::path_exists_follow_junction(&installer) {
        return Err(AppError::NotFound(format!("安装包不存在: {}", path)));
    }

    // 安全校验：仅允许执行 .exe 文件
    match installer.extension().and_then(|e| e.to_str()) {
        Some("exe") => {}
        _ => return Err(AppError::Other("安装包必须是 .exe 文件".to_string())),
    }

    std::process::Command::new(&installer)
        .arg("/S")
        .arg("/R")
        .creation_flags_no_window()
        .spawn()
        .map_err(|e| AppError::Process(format!("启动安装器失败: {}", e)))?;

    // 升级前结束运行中的 daemon：主程序退出后仅有 daemon 常驻、占用
    // javaboot-daemon.exe，若不结束，Windows 会锁定该文件导致安装器无法覆盖
    // resources 里的新版 daemon。daemon 的 Job 不设 KILL_ON_JOB_CLOSE，其托管的
    // java 服务进程不会被连带杀掉，新版 daemon 启动后经崩溃恢复重新接管。
    let killed = crate::ipc::stop_daemon();
    if killed > 0 {
        log::info!("升级前已结束 {} 个旧 daemon 进程", killed);
    }

    log::info!("安装器已启动: {}，即将退出当前应用", installer.display());
    tokio::time::sleep(Duration::from_millis(800)).await;
    app.exit(0);
    Ok(())
}
