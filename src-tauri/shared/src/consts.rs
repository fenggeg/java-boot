//! 跨进程约定的常量：管道名、版本号、心跳与重连参数。

/// Windows 命名管道名（daemon 单实例监听；UI 连接端同此名）。
pub const PIPE_NAME: &str = r"\\.\pipe\javaboot-daemon";

/// daemon 自身版本。UI 握手用；不兼容则 UI 提示升级。
pub const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 与此 daemon 协议兼容的最低的 launcher 版本。
pub const MIN_CLIENT_VERSION: &str = "0.16.0";

/// 线协议版本号：wire 结构变更（字段增删/语义变化）时递增。
pub const PROTOCOL_VERSION: u32 = 1;

/// UI 侧心跳间隔（秒）。
pub const HEARTBEAT_INTERVAL_SECS: u64 = 5;

/// UI 重连指数退避的初始与封顶退避（秒）。
pub const RECONNECT_BASE_MS: u64 = 1000;
pub const RECONNECT_MAX_MS: u64 = 30_000;

/// daemon 空闲自杀：无运行中服务且无 UI 连接持续（秒）。
pub const IDLE_SHUTDOWN_SECS: u64 = 600;

/// 优雅停止超时与端口释放检查相关（秒/毫秒）。
pub const STOP_WAIT_PID_SECS: u64 = 8;
pub const PORT_PROBE_INTERVAL_MS: u64 = 500;
pub const READY_TIMEOUT_SECS: u64 = 300;

/// 日志写库触发阈值：定时(ms) / 条数。
pub const LOG_FLUSH_INTERVAL_MS: u64 = 200;
pub const LOG_FLUSH_THRESHOLD: usize = 500;

/// 日志保留策略。
pub const LOG_RETENTION_DAYS: i64 = 14;
pub const LOG_RUN_MAX_BYTES: i64 = 50 * 1024 * 1024;
pub const LOG_RUN_KEEP_HEAD_TAIL_BYTES: i64 = 5 * 1024 * 1024;