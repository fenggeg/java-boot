pub mod commands;
pub mod db;
pub mod error;
pub mod ipc;
pub mod pom;
pub mod port;
pub mod process;
pub mod project_fs;
pub mod update;
pub mod util;
pub mod watcher;

use std::sync::Arc;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 日志写入文件（%APPDATA%/javaboot-launcher/javaboot.log），便于排查安装器启动时环境缺失等问题
    {
        let log_dir = dirs::data_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("javaboot-launcher");
        let _ = std::fs::create_dir_all(&log_dir);
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_dir.join("javaboot.log"));
        if let Ok(f) = log_file {
            env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
                .target(env_logger::Target::Pipe(Box::new(f)))
                .format_timestamp_secs()
                .try_init()
                .ok();
        } else {
            env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
                .try_init()
                .ok();
        }
    }

    tauri::Builder::default()
        .manage(update::DownloadCancel::default())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_http::init())
        .on_window_event(|window, event| {
            // 应用关闭时停止所有服务
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // 先同步停止所有文件监听：watcher 防抖 worker 是独立线程，
                // 退出竞态中可能恰好触发 trigger_restart → compile_and_start
                // → stop 杀进程，导致 stop_all_on_exit=false 时服务被误杀。
                watcher::get_watch_manager().unwatch_all();
                let cfg = db::load_config().unwrap_or_default();
                if cfg.stop_all_on_exit {
                    let app_handle = window.app_handle().clone();
                    api.prevent_close();
                    tauri::async_runtime::spawn(async move {
                        process::get_manager().stop_all(app_handle.clone()).await.ok();
                        // 用 app.exit 走正常退出流程，确保 Drop/清理执行
                        app_handle.exit(0);
                    });
                } else {
                    let app_handle = window.app_handle().clone();
                    api.prevent_close();
                    tauri::async_runtime::spawn(async move {
                        app_handle.exit(0);
                    });
                }
                // 兜底：如果 async task panic 或 runtime 提前关闭，
                // app.exit(0) 不会执行，窗口被永久阻止关闭。
                // 10 秒后无论如何强制退出进程，避免用户只能通过任务管理器强杀。
                std::thread::spawn(|| {
                    std::thread::sleep(std::time::Duration::from_secs(10));
                    log::warn!("退出超时，强制退出进程");
                    std::process::exit(0);
                });
            }
        })
        .setup(|app| {
            // 最早期：从注册表合并完整 PATH（修复安装器启动时 PATH 不完整导致 git/java/maven 检测失败）
            process::env::merge_registry_path();

            // IPC 客户端：连接/拉起 daemon（UI 崩溃不影响 daemon）
            let ipc_state = ipc::IpcState::spawn();
            app.manage(ipc_state.clone());
            // daemon 事件（日志/状态/监控指标/连接）转发到前端
            ipc_state.forward_to_frontend(app.handle().clone());
            // P4：daemon 事件归一到 service 维度，驱动既有 service://status / service://log
            {
                let app = app.handle().clone();
                let mut rx = ipc_state.events.subscribe();
                tauri::async_runtime::spawn(async move {
                    while let Ok(ev) = rx.recv().await {
                        process::delegate::normalize_event(&app, &ev);
                    }
                });
            }

            // 诊断：打印关键环境变量（排查安装器启动时环境缺失问题）
            {
                log::info!("=== 环境变量诊断 ===");
                log::info!("USERPROFILE = {:?}", std::env::var("USERPROFILE"));
                log::info!("JAVA_HOME = {:?}", std::env::var("JAVA_HOME"));
                log::info!("MAVEN_HOME = {:?}", std::env::var("MAVEN_HOME"));
                log::info!("M2_HOME = {:?}", std::env::var("M2_HOME"));
                let path = std::env::var("PATH").unwrap_or_default();
                log::info!("PATH ({} chars): {}", path.len(), path);
                log::info!("=== 环境变量诊断结束 ===");
            }

            // 初始化数据库
            db::init().map_err(|e| {
                log::error!("数据库初始化失败: {}", e);
                e
            })?;

            // 恢复上次运行中的服务：等 daemon 连上后，在线则经 delegate 重建映射
            // （实时日志随事件恢复，无需重启服务）；离线则回退本地恢复路径
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    let online = handle.state::<Arc<ipc::IpcState>>().inner().is_connected();
                    if online {
                        let _ = process::delegate::rebind(&handle).await;
                    } else {
                        process::get_manager().restore_running_services(&handle);
                    }
                });
            }

            // 初始化文件监听
            let handle = app.handle().clone();
            watcher::get_watch_manager().refresh_all(&handle);

            // 后台预热：为历史服务回填 main_class（旧版本添加的服务可能为空，避免启动阶段现扫）
            // 把 7 秒消耗移到 app 启动后的后台，用户手动启动服务时已 DB 命中。
            // 直接复用 process::build::detect_main_class，避免逻辑重复。
            tauri::async_runtime::spawn_blocking(|| {
                let services = match db::list_services() {
                    Ok(v) => v,
                    Err(_) => return,
                };
                for s in services {
                    if s.main_class.as_deref().map(|m| !m.trim().is_empty()).unwrap_or(false) {
                        continue;
                    }
                    let dir = std::path::PathBuf::from(&s.working_dir);
                    // detect_main_class 内部会回写 DB，忽略错误（服务可能已删除等）
                    let _ = process::build::detect_main_class(&s, &dir);
                }
            });

            // 启动 CPU/内存占用 + 端口定时刷新（集中执行，避免每服务独立轮询）
            // 间隔由 AppConfig.port_refresh_interval_secs 控制（默认 2s）
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    let interval = db::load_config()
                        .map(|c| c.port_refresh_interval_secs)
                        .unwrap_or(2)
                        .max(1);
                    tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
                    let mgr = process::get_manager();
                    mgr.refresh_resource_usage(&handle);
                    mgr.refresh_ports(&handle);
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // project
            commands::list_projects,
            commands::list_services,
            commands::scan_project,
            commands::add_project,
            commands::rescan_project,
            commands::delete_project,
            commands::update_project_env,
            // service
            commands::add_service,
            commands::update_service,
            commands::delete_service,
            commands::toggle_auto_restart,
            // process
            commands::start_service,
            commands::stop_service,
            commands::restart_service,
            commands::compile_and_start,
            commands::recompile_and_start,
            commands::clean_service,
            commands::package_service,
            commands::package_project_services,
            commands::stop_all,
            commands::start_service_with_dependencies,
            commands::start_services_batch,
            commands::get_service_dependencies,
            commands::set_service_dependencies,
            commands::get_runtime,
            commands::get_all_runtimes,
            commands::refresh_port_conflicts,
            // files
            commands::list_files,
            commands::read_project_file,
            commands::write_project_file,
            commands::get_file_abs_path,
            commands::fs_rename,
            commands::fs_copy_entry,
            commands::fs_move_entry,
            commands::reveal_in_file_manager,
            commands::reveal_path_in_file_manager,
            commands::walk_files,
            // config
            commands::get_config,
            commands::save_config,
            // util
            commands::open_in_browser,
            commands::detect_jdks,
            commands::detect_mavens,
            // daemon (IPC 代理)
            commands::daemon_connected,
            commands::daemon_hello,
            commands::daemon_reconcile,
            commands::daemon_ensure_started,
            commands::daemon_spawn,
            commands::daemon_stop,
            commands::daemon_logtail,
            commands::daemon_recovery_list,
            commands::daemon_recovery_takeover,
            commands::daemon_recovery_restart,
            commands::daemon_recovery_ignore,
            commands::daemon_scan_start,
            commands::daemon_scan_cancel,
            // update
            update::download_update,
            update::install_update,
            update::cancel_update,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

