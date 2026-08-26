//! 集成命令终端
//!
//! 每个项目维护一个交互式 shell 会话（管道模式）：
//! - 优先使用 PowerShell 7+（pwsh.exe，PATH 中存在时），回退系统自带的
//!   Windows PowerShell（powershell.exe）
//! - [`terminal_create`]：以项目根目录为 cwd 启动 shell，返回会话 id
//! - [`terminal_write`]：向 shell stdin 写入数据（命令行 + `\r\n`）
//! - [`terminal_kill`]：终止会话并回收资源
//!
//! shell 的 stdout/stderr 以块为单位解码后经 `terminal://out` 事件推送到前端：
//! `{id, chunk, closed}`。进程退出时 `closed=true`。
//!
//! 说明：未使用 ConPTY（避免引入平台依赖），交互式程序（如需要密码输入的）
//! 不受支持；常规 mvn/git/dir 等命令可正常执行。命令回显由前端本地补齐。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use once_cell::sync::Lazy;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStdin};
use tokio::sync::Mutex;

use crate::db;
use crate::error::{AppError, AppResult};
use crate::util::NoWindow;

/// 终端输出事件载荷
#[derive(Clone, Serialize)]
pub struct TerminalChunk {
    /// 会话 id
    pub id: String,
    /// 本次输出的文本块
    pub chunk: String,
    /// 进程是否已退出
    pub closed: bool,
}

struct TerminalSession {
    project_id: String,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    child: Arc<Mutex<Option<Child>>>,
    /// Windows Job Object：终止会话时连带杀掉 shell 启动的整个进程树
    /// （如 shell 里跑着的 mvn / java），避免孤儿进程
    #[allow(dead_code)]
    job: Arc<Mutex<Option<crate::process::job::JobObject>>>,
}

static SESSIONS: Lazy<Mutex<HashMap<String, TerminalSession>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// 解码控制台输出：UTF-8 优先，失败按 GBK 宽容解码（中文 Windows 常见 cp936）
fn decode_console(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => {
            let (cow, _, _) = encoding_rs::GBK.decode(bytes);
            cow.into_owned()
        }
    }
}

/// 在 PATH 中查找可执行文件（跟随 junction）
fn find_in_path(exe: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(exe);
        if crate::util::path_exists_follow_junction(&candidate) {
            return Some(candidate.to_string_lossy().to_string());
        }
    }
    None
}

/// 解析终端 shell：优先 PowerShell 7+（pwsh），回退系统自带的 Windows PowerShell
/// （powershell.exe 在所有 Windows 上必然存在）
fn resolve_shell() -> (String, Vec<String>) {
    let args: Vec<String> = vec![
        // 不打印版权横幅（加快首帧输出）；放开脚本执行策略便于直接跑 ps1
        "-NoLogo".into(),
        "-ExecutionPolicy".into(),
        "Bypass".into(),
    ];
    if let Some(pwsh) = find_in_path("pwsh.exe") {
        return (pwsh, args);
    }
    ("powershell".to_string(), args)
}

/// 创建终端会话，返回会话 id
pub async fn create(app: AppHandle, project_id: &str) -> AppResult<String> {
    let project = db::get_project(project_id)?;
    let cwd = std::path::PathBuf::from(&project.root_path);
    if !cwd.is_dir() {
        return Err(AppError::NotFound(format!(
            "项目根目录不存在: {}",
            cwd.display()
        )));
    }

    let (shell_prog, shell_args) = resolve_shell();
    let mut child = tokio::process::Command::new(&shell_prog)
        .args(&shell_args)
        .current_dir(&cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .creation_flags_no_window()
        .spawn()
        .map_err(|e| AppError::Other(format!("启动终端失败: {}", e)))?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| AppError::Other("无法获取终端 stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::Other("无法获取终端 stdout".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AppError::Other("无法获取终端 stderr".into()))?;

    let id = uuid::Uuid::new_v4().to_string();

    // 进程树托管：把 shell 挂进 Job Object，kill 时整棵树一起终止
    let job = crate::process::job::JobObject::new().ok();
    #[cfg(windows)]
    if let Some(job) = job.as_ref() {
        use windows::Win32::Foundation::HANDLE;
        if let Some(h) = child.raw_handle() {
            let _ = job.assign(HANDLE(h));
        }
    }

    let session = TerminalSession {
        project_id: project_id.to_string(),
        stdin: Arc::new(Mutex::new(Some(stdin))),
        child: Arc::new(Mutex::new(Some(child))),
        job: Arc::new(Mutex::new(job)),
    };

    // 同项目已有旧会话先清理，保持每项目单会话语义
    cleanup_project(&session.project_id).await;

    SESSIONS.lock().await.insert(id.clone(), session);

    // 进程退出标记：两个读流任务共用，EOF 时只发一次 closed 事件
    let exited = Arc::new(AtomicBool::new(false));
    spawn_reader(app.clone(), &id, stdout, exited.clone());
    spawn_reader(app.clone(), &id, stderr, exited);
    Ok(id)
}

/// 启动读流任务：持续读取子进程输出并以事件推送前端
fn spawn_reader(
    app: AppHandle,
    session_id: &str,
    mut stream: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    exited: Arc<AtomicBool>,
) {
    let sid = session_id.to_string();
    tauri::async_runtime::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) => break, // EOF
                Ok(n) => {
                    if exited.load(Ordering::SeqCst) {
                        break;
                    }
                    let chunk = decode_console(&buf[..n]);
                    if !chunk.is_empty() {
                        let _ = app.emit(
                            "terminal://out",
                            TerminalChunk {
                                id: sid.clone(),
                                chunk,
                                closed: false,
                            },
                        );
                    }
                }
                Err(_) => break,
            }
        }
        if exited.swap(true, Ordering::SeqCst) {
            return; // 另一个流已触发收尾
        }
        // 收尾：清空 stdin 句柄、从会话表移除、通知前端
        if let Some(s) = SESSIONS.lock().await.remove(&sid) {
            *s.stdin.lock().await = None;
        }
        let _ = app.emit(
            "terminal://out",
            TerminalChunk {
                id: sid,
                chunk: "\r\n[进程已退出]\r\n".to_string(),
                closed: true,
            },
        );
    });
}

/// 向会话写入数据（用户输入的命令行）
pub async fn write(session_id: &str, data: &str) -> AppResult<()> {
    let sessions = SESSIONS.lock().await;
    let s = sessions
        .get(session_id)
        .ok_or_else(|| AppError::NotFound("终端会话不存在或已退出".into()))?;
    let mut guard = s.stdin.lock().await;
    match guard.as_mut() {
        Some(stdin) => {
            use tokio::io::AsyncWriteExt;
            stdin
                .write_all(data.as_bytes())
                .await
                .map_err(|e| AppError::Other(format!("写入终端失败: {}", e)))?;
            stdin
                .flush()
                .await
                .map_err(|e| AppError::Other(format!("写入终端失败: {}", e)))?;
            Ok(())
        }
        None => Err(AppError::Other("终端已退出".into())),
    }
}

/// 终止指定会话（连带其启动的子进程树）
pub async fn kill(session_id: &str) -> AppResult<()> {
    let s = SESSIONS
        .lock()
        .await
        .remove(session_id)
        .ok_or_else(|| AppError::NotFound("终端会话不存在".into()))?;
    *s.stdin.lock().await = None;
    if let Some(mut j) = s.job.lock().await.take() {
        // 先终止整个进程树（含 cmd 的子孙进程），再收尾 cmd 句柄
        j.kill();
    }
    if let Some(mut c) = s.child.lock().await.take() {
        let _ = c.start_kill();
        let _ = c.wait().await;
    }
    Ok(())
}

/// 清理某项目的旧会话（同项目重复创建时调用）
async fn cleanup_project(project_id: &str) {
    let victims: Vec<String> = SESSIONS
        .lock()
        .await
        .iter()
        .filter(|(_, s)| s.project_id == project_id)
        .map(|(k, _)| k.clone())
        .collect();
    for v in victims {
        let _ = kill(&v).await;
    }
}

/// 应用退出时终止所有终端会话，防止残留 cmd.exe 进程
pub async fn kill_all() {
    let ids: Vec<String> = SESSIONS.lock().await.keys().cloned().collect();
    for id in ids {
        let _ = kill(&id).await;
    }
}
