//! 应用共享状态（AppState）与各服务的装配。
//!
//! daemon 的单一事实来源：database、进程托管、日志管线、Job Object 的汇聚点。

use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use tokio::sync::broadcast;

use jb_core::protocol::{event, LogAppendEvent, Message, Notification};

use crate::job::JobObject;
use crate::log_pipe::LogPipeline;
use crate::monitor::MonitorService;
use crate::proc::ProcService;
use crate::scan::ScanService;
use crate::store::Store;

pub struct AppState {
    pub store: Arc<Store>,
    pub procs: Arc<ProcService>,
    pub scan: Arc<ScanService>,
    pub log: Arc<LogPipeline>,
    /// P3 监控：周期采样 CPU/内存并推送 `proc.metrics`。
    pub monitor: Arc<MonitorService>,
    /// 服务端事件总线：session 订阅、各服务在此发 `Message::Notification`。
    pub events: broadcast::Sender<Message>,
    /// 当前活跃 session 数（用于空闲自杀判据）。
    pub sessions: AtomicUsize,
    /// 最近一次「有 UI 活动」的时刻（连接 or 收到消息）。
    pub last_activity: Mutex<Instant>,
}

impl AppState {
    pub fn new() -> AppState {
        // 先生成事件总线，便于各服务共享（日志转发 / proc.status 通知）
        let (events_tx, _) = broadcast::channel::<Message>(1024);

        // 装配各服务
        let store = Store::open(&Store::default_db_path()).expect("打开存储失败");
        let log = LogPipeline::spawn(Arc::clone(&store));
        let job = JobObject::create().expect("创建 Job Object 失败");
        let procs = ProcService::new(
            Arc::clone(&store),
            Arc::clone(&log),
            Arc::new(job),
            events_tx.clone(),
        );
        let scan = ScanService::new(Arc::clone(&store), events_tx.clone());
        let monitor = MonitorService::new(Arc::clone(&procs), events_tx.clone());
        monitor.spawn();

        // 把 LogPipeline 的日志事件转发到服务端事件总线，便于统一订阅。
        {
            let bus = events_tx.clone();
            let mut log_rx = log.events.subscribe();
            tokio::spawn(async move {
                loop {
                    match log_rx.recv().await {
                        Ok(line) => {
                            let notif = Notification::named(
                                event::LOG_APPEND,
                                &LogAppendEvent { line },
                            );
                            let _ = bus.send(Message::Notification(notif));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            log::warn!("日志事件 lagged，丢弃 {n} 条");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
        }

        AppState {
            store,
            procs,
            scan,
            log,
            monitor,
            events: events_tx,
            sessions: AtomicUsize::new(0),
            last_activity: Mutex::new(Instant::now()),
        }
    }
}