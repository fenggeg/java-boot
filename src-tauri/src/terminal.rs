//! 集成命令终端（ConPTY 全功能模式）
//!
//! 每个项目维护一个交互式 shell 会话，基于 [portable-pty]（Windows 上走
//! ConPTY 伪控制台）：
//! - 完整终端能力：ANSI/VT 序列、彩色输出、光标控制、交互式程序
//!   （python REPL、ssh、需要密码输入的命令均可正常使用）
//! - 前端 xterm.js 直连：键盘输入经 `terminal_write` 原样写入 PTY，
//!   回显/行编辑/历史（PSReadLine）由 shell 自身完成
//! - [`terminal_resize`]：跟随前端窗口尺寸调整伪终端行列数
//!
//! 输出以块为单位做增量 UTF-8 解码后经 `terminal://out` 事件推送前端：
//! `{id, chunk, closed}`。进程退出时 `closed=true`。

use std::collections::HashMap;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use once_cell::sync::Lazy;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;

use crate::db;
use crate::error::{AppError, AppResult};

/// 终端输出事件载荷
#[derive(Clone, Serialize)]
pub struct TerminalChunk {
    /// 会话 id
    pub id: String,
    /// 本次输出的文本块（已解码 UTF-8）
    pub chunk: String,
    /// 进程是否已退出
    pub closed: bool,
}

struct TerminalSession {
    project_id: String,
    master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    writer: Arc<Mutex<Option<Box<dyn std::io::Write + Send>>>>,
    child: Arc<Mutex<Option<Box<dyn Child + Send + Sync>>>>,
}

static SESSIONS: Lazy<Mutex<HashMap<String, TerminalSession>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

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

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| AppError::Other(format!("创建伪终端失败: {}", e)))?;

    let (shell_prog, shell_args) = resolve_shell();
    let mut cmd = CommandBuilder::new(&shell_prog);
    cmd.cwd(&cwd);
    cmd.args(shell_args.iter().map(|s| s.as_str()));
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| AppError::Other(format!("启动终端失败: {}", e)))?;

    // slave 句柄用完即弃：父进程持有会导致 ConPTY 输出异常
    drop(pair.slave);

    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| AppError::Other(format!("获取终端输出流失败: {}", e)))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| AppError::Other(format!("获取终端输入流失败: {}", e)))?;

    let id = uuid::Uuid::new_v4().to_string();

    let session = TerminalSession {
        project_id: project_id.to_string(),
        master: Arc::new(Mutex::new(Some(pair.master))),
        writer: Arc::new(Mutex::new(Some(writer))),
        child: Arc::new(Mutex::new(Some(child))),
    };

    // 同项目已有旧会话先清理，保持每项目单会话语义
    cleanup_project(&session.project_id).await;

    SESSIONS.lock().await.insert(id.clone(), session);

    spawn_reader(app, &id, reader);
    Ok(id)
}

/// 启动读线程：持续读取 PTY 输出并推送到前端。
///
/// ConPTY 输出恒为 UTF-8；多字节字符可能被拆在两次读取中，
/// 使用有状态解码器保证不产生乱码。EOF 时发送 closed 事件并清理会话。
fn spawn_reader(
    app: AppHandle,
    session_id: &str,
    mut reader: Box<dyn Read + Send>,
) {
    let sid = session_id.to_string();
    let exited = Arc::new(AtomicBool::new(false));
    let exited_clone = exited.clone();
    std::thread::spawn(move || {
        let mut dec = encoding_rs::UTF_8.new_decoder();
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    if exited.load(Ordering::SeqCst) {
                        break;
                    }
                    let mut chunk = String::with_capacity(n * 2);
                    let _ = dec.decode_to_string(&buf[..n], &mut chunk, false);
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
        if exited_clone.swap(true, Ordering::SeqCst) {
            return; // 已收尾
        }
        // 收尾：从会话表移除、关闭句柄、通知前端
        if let Some(s) = SESSIONS.blocking_lock().remove(&sid) {
            *s.writer.blocking_lock() = None;
            *s.master.blocking_lock() = None;
            if let Some(mut c) = s.child.blocking_lock().take() {
                let _ = c.wait();
            }
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

/// 向会话写入原始输入（xterm.js 键盘数据原样透传）
pub async fn write(session_id: &str, data: &str) -> AppResult<()> {
    let sessions = SESSIONS.lock().await;
    let s = sessions
        .get(session_id)
        .ok_or_else(|| AppError::NotFound("终端会话不存在或已退出".into()))?;
    let mut guard = s.writer.lock().await;
    match guard.as_mut() {
        Some(writer) => {
            use std::io::Write;
            writer
                .write_all(data.as_bytes())
                .map_err(|e| AppError::Other(format!("写入终端失败: {}", e)))?;
            writer
                .flush()
                .map_err(|e| AppError::Other(format!("写入终端失败: {}", e)))?;
            Ok(())
        }
        None => Err(AppError::Other("终端已退出".into())),
    }
}

/// 调整伪终端尺寸（前端 xterm.js fit 后调用）
pub async fn resize(session_id: &str, cols: u16, rows: u16) -> AppResult<()> {
    let sessions = SESSIONS.lock().await;
    let s = sessions
        .get(session_id)
        .ok_or_else(|| AppError::NotFound("终端会话不存在或已退出".into()))?;
    let guard = s.master.lock().await;
    match guard.as_ref() {
        Some(master) => master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| AppError::Other(format!("调整终端尺寸失败: {}", e))),
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
    *s.writer.lock().await = None;
    *s.master.lock().await = None;
    if let Some(mut c) = s.child.lock().await.take() {
        // 先按 PID 杀整棵进程树（shell 里跑的 mvn/java 一并结束），再回收句柄
        if let Some(pid) = c.process_id() {
            crate::process::manager::kill_process_tree_by_pid(pid);
        }
        let _ = c.kill();
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

/// 应用退出时终止所有终端会话，防止残留 shell 进程
///
/// **不杀进程树**：用户可能在终端里手动启动了服务（mvn/java），
/// 这些进程不应随应用退出被杀（由 stop_all_on_exit 配置控制服务生死）。
/// 只杀 shell 本身（c.kill），shell 的子进程会因 ConPTY 关闭而失去终端，
/// 但进程本身继续运行。
pub async fn kill_all() {
    let ids: Vec<String> = SESSIONS.lock().await.keys().cloned().collect();
    for id in ids {
        let s = SESSIONS.lock().await.remove(&id);
        if let Some(s) = s {
            *s.writer.lock().await = None;
            *s.master.lock().await = None;
            if let Some(mut c) = s.child.lock().await.take() {
                // 只杀 shell 进程本身，不杀进程树
                let _ = c.kill();
            }
        }
    }
}
