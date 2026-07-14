pub mod commands;
pub mod db;
pub mod error;
pub mod git;
pub mod pom;
pub mod port;
pub mod process;
pub mod watcher;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init()
        .ok();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .on_window_event(|window, event| {
            // 应用关闭时停止所有服务
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let cfg = db::load_config().unwrap_or_default();
                if cfg.stop_all_on_exit {
                    let app_handle = window.app_handle().clone();
                    api.prevent_close();
                    tauri::async_runtime::spawn(async move {
                        process::get_manager().stop_all(app_handle).await.ok();
                        std::process::exit(0);
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

            // 启动 CPU/内存占用定时刷新
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    process::get_manager().refresh_resource_usage(&handle);
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

