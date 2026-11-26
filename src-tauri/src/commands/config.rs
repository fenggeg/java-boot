//! 全局配置。
use crate::db;
use crate::db::models::AppConfig;
use crate::error::AppResult;

// ============================ Config ============================

#[tauri::command]
pub fn get_config() -> AppResult<AppConfig> {
    db::load_config()
}

#[tauri::command]
pub fn save_config(config: AppConfig) -> AppResult<()> {
    db::save_config(&config)
}

