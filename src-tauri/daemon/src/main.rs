//! javaboot-daemon 常驻守护进程入口。
//!
//! - 单实例（Named Pipe 原生互斥，创建管道失败即已有人在跑，见 `server::run`）
//! - 多线程 tokio runtime；阻塞 IO 一律走 `spawn_blocking`
//! - 生命周期：随用拉起、空闲自杀（见 `server::spawn_idle_watchdog`）

mod app;
mod error;
mod job;
mod log_pipe;
mod monitor;
mod proc;
mod scan;
mod server;
mod store;

fn main() {
    init_logger();
    log::info!(
        "javaboot-daemon 启动, version = {}, protocol = {}",
        jb_core::consts::DAEMON_VERSION,
        jb_core::consts::PROTOCOL_VERSION
    );

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("创建 tokio runtime 失败: {e}");
            std::process::exit(1);
        }
    };

    // 单实例：管道已有人占用（能连上）说明另一个 daemon 在跑，直接退出。
    let already_running = tokio::net::windows::named_pipe::ClientOptions::new()
        .open(jb_core::consts::PIPE_NAME)
        .is_ok();
    if already_running {
        log::info!("检测到 daemon 已在运行，本实例退出");
        return;
    }

    // AppState::new() 内部会 spawn tokio 任务，必须在 runtime 上下文内构建。
    let state = rt.block_on(async { std::sync::Arc::new(app::AppState::new()) });

    rt.block_on(async {
        if let Err(e) = server::run(state).await {
            log::error!("daemon 主循环异常退出: {e}");
            std::process::exit(1);
        }
    });
}

fn init_logger() {
    let level = std::env::var("JB_DAEMON_LOG").unwrap_or_else(|_| "info".to_string());
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level))
        .format_timestamp_millis()
        .try_init()
        .ok();
}