//! 日志采集管线（ADR-0001 决策 5 / R6）。
//!
//! 流程：子进程 stdout/stderr 管道 reader → `mpsc` → 后台攒批
//! → 200ms 定时 / 500 条阈值 触发的 `spawn_blocking` 双写
//!   - `service_log`（SQLite 批量，prepare_cached）
//!   - `.javaboot/<module>.log` 文件镜像（append）
//! → 同时经 `broadcast` 实时推送给已连接客户端（`log.append` 事件）。
//!
//! 保证：UI 断连期间日志照常落库；文件镜像与数据库都无阻塞 async 上下文。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::{broadcast, mpsc};

use jb_core::consts as C;
use jb_core::model::{LogLine, Stream};

use crate::store::Store;

/// 镜像文件句柄缓存：run_id → 已打开的追加文件。
type MirrorMap = Mutex<HashMap<i64, std::fs::File>>;
/// run 的镜像补充信息。
type MirrorInfoMap = Mutex<HashMap<i64, MirrorInfo>>;

struct MirrorInfo {
    /// 镜像文件所在目录（工作目录 / `.javaboot`）。
    dir: PathBuf,
    /// 显示名（模块名），用于日志文件名。
    module: String,
}

pub struct LogPipeline {
    /// 子进程管道 reader 写入入口。
    pub tx: mpsc::UnboundedSender<LogLine>,
    /// 实时事件广播（客户端订阅 `log.append`）。
    pub events: broadcast::Sender<LogLine>,
    store: Arc<Store>,
    files: Arc<MirrorMap>,
    infos: Arc<MirrorInfoMap>,
}

impl LogPipeline {
    /// 创建管线并启动后台攒批任务。
    pub fn spawn(store: Arc<Store>) -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel::<LogLine>();
        let (events, _) = broadcast::channel::<LogLine>(8192);
        let pipe = Arc::new(LogPipeline {
            tx,
            events,
            store,
            files: Arc::new(MirrorMap::default()),
            infos: Arc::new(MirrorInfoMap::default()),
        });
        let cloned = pipe.clone();
        tokio::spawn(async move {
            cloned.drain_loop(rx).await;
        });
        pipe
    }

    /// 注册一个 run 的镜像目标。返回该 run 的镜像文件路径（用于回写 `process_spec.log_file`）。
    pub fn attach(&self, run_id: i64, working_dir: &str, module: &str) -> PathBuf {
        let base = PathBuf::from(working_dir);
        let dir = base.join(jb_core::model::LOG_MIRROR_DIR);
        let path = dir.join(format!("{}.log", sanitize(module)));
        self.infos.lock().insert(
            run_id,
            MirrorInfo { dir, module: module.to_string() },
        );
        path
    }

    async fn drain_loop(&self, mut rx: mpsc::UnboundedReceiver<LogLine>) {
        let mut buf: Vec<LogLine> = Vec::new();
        loop {
            let timer = tokio::time::sleep(std::time::Duration::from_millis(C::LOG_FLUSH_INTERVAL_MS));
            tokio::pin!(timer);
            // 攒批：直到超时或达到阈值
            loop {
                tokio::select! {
                    line = rx.recv() => match line {
                        Some(l) => {
                            // 实时推送（客户端在旧 seq 跳过，靠 vet 对齐）
                            let _ = self.events.send(l.clone());
                            buf.push(l);
                        }
                        None => {
                            self.flush_batch(&mut buf).await;
                            return;
                        }
                    },
                    _ = &mut timer => break,
                }
                if buf.len() >= C::LOG_FLUSH_THRESHOLD {
                    break;
                }
            }
            if !buf.is_empty() {
                self.flush_batch(&mut buf).await;
            }
        }
    }

    async fn flush_batch(&self, buf: &mut Vec<LogLine>) {
        if buf.is_empty() {
            return;
        }
        let lines: Vec<LogLine> = std::mem::take(buf);
        // 落库：write_logs 内部已 spawn_blocking，不在 async 上下文做阻塞 IO。
        if let Err(e) = self.store.clone().write_logs(lines.clone()).await {
            log::warn!("落库日志失败({} 条): {}", lines.len(), e);
        }
        // 文件镜像：独立阻塞线程内追加写。
        let files_map = Arc::clone(&self.files);
        let infos_map = Arc::clone(&self.infos);
        tokio::task::spawn_blocking(move || {
            Self::write_mirror(&files_map, &infos_map, &lines);
        });
    }

    /// 阻塞线程内执行：把一批日志 append 到各自镜像文件。
    fn write_mirror(
        files: &Arc<MirrorMap>,
        infos: &Arc<MirrorInfoMap>,
        lines: &[LogLine],
    ) {
        let info_guard = infos.lock();
        for l in lines {
            let Some(info) = info_guard.get(&l.run_id) else {
                continue;
            };
            let _ = std::fs::create_dir_all(&info.dir);
            let file_path = info.dir.join(format!("{}.log", sanitize(&info.module)));
            let mut files_guard = files.lock();
            if !files_guard.contains_key(&l.run_id) {
                if let Ok(f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&file_path)
                {
                    files_guard.insert(l.run_id, f);
                }
            }
            let Some(f) = files_guard.get_mut(&l.run_id) else {
                continue;
            };
            let _ = write_line(f, &l.body);
        }
    }
}

fn write_line(f: &mut std::fs::File, body: &str) -> std::io::Result<()> {
    use std::io::Write;
    let stream_tag = body; // 镜像只存正文
    f.write_all(stream_tag.as_bytes())?;
    f.write_all(b"\n")
}

/// 镜像仅用于展示，去掉换行等会导致多行的字符。
pub fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' })
        .collect()
}

// ---- 供其他模块便捷使用 ----

/// 由 (stream, ts, body) 构造一条已分配 seq 的日志行（seq 由调用方递增维护）。
pub fn make_line(run_id: i64, seq: i64, ts: i64, stream: Stream, body: String) -> LogLine {
    LogLine { run_id, seq, ts, stream, level: None, body }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_name() {
        assert_eq!(sanitize("user-service_1.2"), "user-service_1.2");
        assert_eq!(sanitize("a b\\c:d?"), "a_b_c_d_");
    }
}