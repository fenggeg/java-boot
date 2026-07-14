pub mod models;
pub mod schema;

use std::sync::Mutex;

use chrono::Utc;
use once_cell::sync::Lazy;
use rusqlite::Connection;
use uuid::Uuid;

use crate::error::AppResult;

use models::{AppConfig, Project, Service};

/// 全局数据库连接（Mutex 包裹，Tauri 命令可跨线程访问）
static DB: Lazy<Mutex<Option<Connection>>> = Lazy::new(|| Mutex::new(None));

/// 初始化数据库，在 app setup 阶段调用一次
pub fn init() -> AppResult<()> {
    let db_dir = dirs::data_dir()
        .ok_or_else(|| crate::error::AppError::Other("无法定位数据目录".into()))?
        .join("javaboot-launcher");
    std::fs::create_dir_all(&db_dir)?;
    let db_path = db_dir.join("data.db");
    log::info!("数据库路径: {}", db_path.display());

    let conn = Connection::open(db_path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    schema::run_migrations(&conn)?;

    *DB.lock().unwrap() = Some(conn);
    Ok(())
}

fn with_conn<F, R>(f: F) -> AppResult<R>
where
    F: FnOnce(&Connection) -> AppResult<R>,
{
    let guard = DB.lock().unwrap();
    let conn = guard.as_ref().ok_or_else(|| {
        crate::error::AppError::Other("数据库未初始化".into())
    })?;
    f(conn)
}

// ============================ Project CRUD ============================

const PROJECT_COLS: &str = "id, name, root_path, git_available, java_home, maven_home, created_at";

macro_rules! row_to_project {
    ($row:expr) => {
        Project {
            id: $row.get(0)?,
            name: $row.get(1)?,
            root_path: $row.get(2)?,
            git_available: $row.get::<_, i64>(3)? != 0,
            java_home: $row.get(4)?,
            maven_home: $row.get(5)?,
            created_at: $row.get(6)?,
        }
    };
}

pub fn list_projects() -> AppResult<Vec<Project>> {
    with_conn(|conn| {
        let sql = format!("SELECT {} FROM projects ORDER BY created_at", PROJECT_COLS);
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| Ok(row_to_project!(row)))?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

pub fn get_project(id: &str) -> AppResult<Project> {
    with_conn(|conn| {
        let sql = format!("SELECT {} FROM projects WHERE id = ?1", PROJECT_COLS);
        let mut stmt = conn.prepare(&sql)?;
        let p = stmt.query_row(rusqlite::params![id], |row| Ok(row_to_project!(row)))?;
        Ok(p)
    })
}

pub fn insert_project(name: &str, root_path: &str, git_available: bool) -> AppResult<Project> {
    let project = Project {
        id: Uuid::new_v4().to_string(),
        name: name.to_string(),
        root_path: root_path.to_string(),
        git_available,
        java_home: None,
        maven_home: None,
        created_at: Utc::now().to_rfc3339(),
    };
    with_conn(|conn| {
        conn.execute(
            "INSERT INTO projects (id, name, root_path, git_available, java_home, maven_home, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                project.id, project.name, project.root_path, project.git_available as i32,
                project.java_home, project.maven_home, project.created_at
            ],
        )?;
        Ok(())
    })?;
    Ok(project)
}

/// 按根路径找已存在的项目
pub fn find_project_by_path(root_path: &str) -> AppResult<Option<Project>> {
    with_conn(|conn| {
        let sql = format!("SELECT {} FROM projects WHERE root_path = ?1", PROJECT_COLS);
        let mut stmt = conn.prepare(&sql)?;
        let res = stmt.query_row(rusqlite::params![root_path], |row| Ok(row_to_project!(row)));
        match res {
            Ok(p) => Ok(Some(p)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    })
}

pub fn delete_project(id: &str) -> AppResult<()> {
    with_conn(|conn| {
        // 先删关联服务
        conn.execute("DELETE FROM services WHERE project_id = ?1", rusqlite::params![id])?;
        conn.execute("DELETE FROM projects WHERE id = ?1", rusqlite::params![id])?;
        Ok(())
    })
}

// ============================ Service CRUD ============================

const SERVICE_COLS: &str = "id, name, pom_path, working_dir, project_id, auto_restart, maven_opts, profiles, created_at";

macro_rules! row_to_service {
    ($row:expr) => {
        Service {
            id: $row.get(0)?,
            name: $row.get(1)?,
            pom_path: $row.get(2)?,
            working_dir: $row.get(3)?,
            project_id: $row.get(4)?,
            auto_restart: $row.get::<_, i64>(5)? != 0,
            maven_opts: $row.get(6)?,
            profiles: $row.get(7)?,
            created_at: $row.get(8)?,
        }
    };
}

pub fn list_services() -> AppResult<Vec<Service>> {
    with_conn(|conn| {
        let sql = format!("SELECT {} FROM services ORDER BY created_at", SERVICE_COLS);
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| Ok(row_to_service!(row)))?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

pub fn list_services_by_project(project_id: &str) -> AppResult<Vec<Service>> {
    with_conn(|conn| {
        let sql = format!("SELECT {} FROM services WHERE project_id = ?1 ORDER BY created_at", SERVICE_COLS);
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![project_id], |row| Ok(row_to_service!(row)))?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

pub fn get_service(id: &str) -> AppResult<Service> {
    with_conn(|conn| {
        let sql = format!("SELECT {} FROM services WHERE id = ?1", SERVICE_COLS);
        let mut stmt = conn.prepare(&sql)?;
        let s = stmt.query_row(rusqlite::params![id], |row| Ok(row_to_service!(row)))?;
        Ok(s)
    })
}

pub fn insert_service(
    name: &str,
    pom_path: &str,
    working_dir: &str,
    project_id: Option<&str>,
) -> AppResult<Service> {
    let service = Service {
        id: Uuid::new_v4().to_string(),
        name: name.to_string(),
        pom_path: pom_path.to_string(),
        working_dir: working_dir.to_string(),
        project_id: project_id.map(String::from),
        auto_restart: false,
        maven_opts: None,
        profiles: None,
        created_at: Utc::now().to_rfc3339(),
    };
    with_conn(|conn| {
        conn.execute(
            "INSERT INTO services (id, name, pom_path, working_dir, project_id, auto_restart, maven_opts, profiles, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                service.id, service.name, service.pom_path, service.working_dir,
                service.project_id, service.auto_restart as i32,
                service.maven_opts, service.profiles, service.created_at
            ],
        )?;
        Ok(())
    })?;
    Ok(service)
}

/// 判断 pom_path 是否已添加为服务
pub fn find_service_by_pom(pom_path: &str) -> AppResult<Option<Service>> {
    with_conn(|conn| {
        let sql = format!("SELECT {} FROM services WHERE pom_path = ?1", SERVICE_COLS);
        let mut stmt = conn.prepare(&sql)?;
        let res = stmt.query_row(rusqlite::params![pom_path], |row| Ok(row_to_service!(row)));
        match res {
            Ok(s) => Ok(Some(s)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    })
}

pub fn update_service(
    id: &str,
    name: Option<&str>,
    auto_restart: Option<bool>,
    maven_opts: Option<Option<&str>>,
    profiles: Option<Option<&str>>,
) -> AppResult<()> {
    with_conn(|conn| {
        if let Some(n) = name {
            conn.execute(
                "UPDATE services SET name = ?1 WHERE id = ?2",
                rusqlite::params![n, id],
            )?;
        }
        if let Some(ar) = auto_restart {
            conn.execute(
                "UPDATE services SET auto_restart = ?1 WHERE id = ?2",
                rusqlite::params![ar as i32, id],
            )?;
        }
        if let Some(mo) = maven_opts {
            conn.execute(
                "UPDATE services SET maven_opts = ?1 WHERE id = ?2",
                rusqlite::params![mo, id],
            )?;
        }
        if let Some(pf) = profiles {
            conn.execute(
                "UPDATE services SET profiles = ?1 WHERE id = ?2",
                rusqlite::params![pf, id],
            )?;
        }
        Ok(())
    })
}

/// 更新项目级 JDK / Maven 配置
pub fn update_project_env(
    id: &str,
    java_home: Option<Option<&str>>,
    maven_home: Option<Option<&str>>,
) -> AppResult<()> {
    with_conn(|conn| {
        if let Some(jh) = java_home {
            conn.execute(
                "UPDATE projects SET java_home = ?1 WHERE id = ?2",
                rusqlite::params![jh, id],
            )?;
        }
        if let Some(mh) = maven_home {
            conn.execute(
                "UPDATE projects SET maven_home = ?1 WHERE id = ?2",
                rusqlite::params![mh, id],
            )?;
        }
        Ok(())
    })
}

pub fn delete_service(id: &str) -> AppResult<()> {
    with_conn(|conn| {
        conn.execute("DELETE FROM services WHERE id = ?1", rusqlite::params![id])?;
        conn.execute(
            "DELETE FROM service_run_pids WHERE service_id = ?1",
            rusqlite::params![id],
        )?;
        Ok(())
    })
}

// ============================ Config ============================

pub fn load_config() -> AppResult<AppConfig> {
    let mut cfg = AppConfig::default();
    with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT key, value FROM app_config")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for r in rows {
            let (k, v) = r?;
            match k.as_str() {
                "port_refresh_interval_secs" => {
                    cfg.port_refresh_interval_secs = v.parse().unwrap_or(2)
                }
                "stop_on_compile_fail" => cfg.stop_on_compile_fail = v == "true",
                "auto_restart_debounce_secs" => {
                    cfg.auto_restart_debounce_secs = v.parse().unwrap_or(3)
                }
                "log_buffer_lines" => cfg.log_buffer_lines = v.parse().unwrap_or(10000),
                "stop_all_on_exit" => cfg.stop_all_on_exit = v == "true",
                _ => {}
            }
        }
        Ok(())
    })?;
    Ok(cfg)
}

pub fn save_config(cfg: &AppConfig) -> AppResult<()> {
    with_conn(|conn| {
        let pairs = [
            ("port_refresh_interval_secs", cfg.port_refresh_interval_secs.to_string()),
            ("stop_on_compile_fail", cfg.stop_on_compile_fail.to_string()),
            ("auto_restart_debounce_secs", cfg.auto_restart_debounce_secs.to_string()),
            ("log_buffer_lines", cfg.log_buffer_lines.to_string()),
            ("stop_all_on_exit", cfg.stop_all_on_exit.to_string()),
        ];
        for (k, v) in pairs {
            conn.execute(
                "INSERT INTO app_config (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
                rusqlite::params![k, v],
            )?;
        }
        Ok(())
    })
}

// ============================ Run PID tracking ============================

pub fn save_run_pid(service_id: &str, pid: u32) -> AppResult<()> {
    with_conn(|conn| {
        conn.execute(
            "INSERT INTO service_run_pids (service_id, pid, started_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(service_id) DO UPDATE SET pid = ?2, started_at = ?3",
            rusqlite::params![service_id, pid, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    })
}

pub fn clear_run_pid(service_id: &str) -> AppResult<()> {
    with_conn(|conn| {
        conn.execute(
            "DELETE FROM service_run_pids WHERE service_id = ?1",
            rusqlite::params![service_id],
        )?;
        Ok(())
    })
}

/// 加载所有持久化的运行 PID
pub fn load_all_run_pids() -> AppResult<Vec<(String, u32, String)>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT service_id, pid, started_at FROM service_run_pids",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}
