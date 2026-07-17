pub mod commands;
pub mod db;
pub mod error;
pub mod git;
pub mod pom;
pub mod port;
pub mod process;
pub mod util;
pub mod watcher;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init()
        .ok();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .on_window_event(|window, event| {
            // 应用关闭时停止所有服务
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let cfg = db::load_config().unwrap_or_default();
                if cfg.stop_all_on_exit {
                    let app_handle = window.app_handle().clone();
                    api.prevent_close();
                    tauri::async_runtime::spawn(async move {
                        process::get_manager().stop_all(app_handle.clone()).await.ok();
                        // 用 app.exit 走正常退出流程，确保 Drop/清理执行
                        app_handle.exit(0);
                    });
                }
            }
        })
        .setup(|app| {
            // 初始化数据库
            db::init().map_err(|e| {
                log::error!("数据库初始化失败: {}", e);
                e
            })?;

            // 恢复上次运行中的服务
            let handle = app.handle().clone();
            process::get_manager().restore_running_services(&handle);

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
            commands::stop_all,
            commands::get_runtime,
            commands::get_all_runtimes,
            commands::refresh_port_conflicts,
            // git
            commands::git_available,
            commands::git_pull,
            commands::git_pull_and_restart,
            // config
            commands::get_config,
            commands::save_config,
            // util
            commands::open_in_browser,
            commands::detect_jdks,
            commands::detect_mavens,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

