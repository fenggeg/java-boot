//! 进程：启动 / 停止 / 编译 / 打包 / 运行时查询 / 端口冲突刷新。
use tauri::AppHandle;
use crate::db;
use crate::db::models::ServiceRuntime;
use crate::error::AppResult;
use crate::process;

// ============================ Process ============================

#[tauri::command]
pub async fn start_service(id: String, app: AppHandle) -> AppResult<()> {
    let service = db::get_service(&id)?;
    process::get_manager().start(app, service).await
}

#[tauri::command]
pub async fn stop_service(id: String, app: AppHandle) -> AppResult<()> {
    process::get_manager().stop(app, &id).await
}

#[tauri::command]
pub async fn restart_service(id: String, app: AppHandle) -> AppResult<()> {
    let service = db::get_service(&id)?;
    process::get_manager().restart(app, service).await
}

#[tauri::command]
pub async fn compile_and_start(id: String, app: AppHandle) -> AppResult<()> {
    let service = db::get_service(&id)?;
    process::get_manager().compile_and_start(app, service).await
}

#[tauri::command]
pub async fn recompile_and_start(id: String, app: AppHandle) -> AppResult<()> {
    let service = db::get_service(&id)?;
    process::get_manager().recompile_and_start(app, service).await
}

/// 清理服务编译产物（mvn clean），不重新启动
#[tauri::command]
pub async fn clean_service(id: String, app: AppHandle) -> AppResult<()> {
    let service = db::get_service(&id)?;
    process::get_manager().clean_service(app, service).await
}

/// 打包单个服务（mvn clean package -DskipTests），生成可执行 jar
#[tauri::command]
pub async fn package_service(id: String, app: AppHandle) -> AppResult<process::manager::PackageResult> {
    let service = db::get_service(&id)?;
    process::get_manager().package_service(app, service).await
}

/// 批量打包项目下所有已添加的服务（逐个打包，避免资源争抢）
#[tauri::command]
pub async fn package_project_services(
    ids: Vec<String>,
    app: AppHandle,
) -> AppResult<process::manager::BatchPackageResult> {
    process::get_manager().package_project_services(app, &ids).await
}

#[tauri::command]
pub async fn stop_all(app: AppHandle) -> AppResult<()> {
    process::get_manager().stop_all(app).await
}

/// 带依赖启动：按拓扑序先启动依赖，再启动目标服务
#[tauri::command]
pub async fn start_service_with_dependencies(id: String, app: AppHandle) -> AppResult<()> {
    let service = db::get_service(&id)?;
    process::get_manager().start_with_dependencies(app, service).await
}

/// 批量启动多个服务（一键启动项目下所有服务，含依赖编排）
#[tauri::command]
pub async fn start_services_batch(
    ids: Vec<String>,
    app: AppHandle,
) -> AppResult<process::manager::BatchStartResult> {
    process::get_manager().start_services_batch(app, &ids).await
}
/// 查询服务的直接依赖列表
#[tauri::command]
pub fn get_service_dependencies(id: String) -> AppResult<Vec<String>> {
    db::list_dependencies(&id)
}

/// 设置服务的依赖列表（全量替换）
#[tauri::command]
pub fn set_service_dependencies(id: String, depends_on_ids: Vec<String>) -> AppResult<()> {
    db::set_dependencies(&id, &depends_on_ids)
}

#[tauri::command]
pub fn get_runtime(id: String) -> ServiceRuntime {
    process::get_manager().get_runtime(&id)
}

#[tauri::command]
pub fn get_all_runtimes() -> Vec<ServiceRuntime> {
    process::get_manager().all_runtimes()
}

#[tauri::command]
pub fn refresh_port_conflicts(app: AppHandle) {
    process::get_manager().refresh_port_conflicts(&app);
}

