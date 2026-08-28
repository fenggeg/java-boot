pub mod models;
pub mod schema;

use chrono::Utc;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use std::sync::OnceLock;
use uuid::Uuid;

use crate::error::AppResult;

use models::{AppConfig, Project, Service};

// 依赖关系 pair（service_id 依赖 depends_on）
pub struct Dependency {
    pub service_id: String,
    pub depends_on: String,
}

/// 全局数据库连接池（r2d2 管理的多连接池，支持并发读取）
static DB: OnceLock<Pool<SqliteConnectionManager>> = OnceLock::new();

/// 初始化数据库，在 app setup 阶段调用一次
pub fn init() -> AppResult<()> {
    let db_dir = dirs::data_dir()
        .ok_or_else(|| crate::error::AppError::Other("无法定位数据目录".into()))?
        .join("javaboot-launcher");
    std::fs::create_dir_all(&db_dir)?;
    let db_path = db_dir.join("data.db");
    log::info!("数据库路径: {}", db_path.display());

    // 先用单连接做迁移，再建连接池
    let conn = rusqlite::Connection::open(&db_path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    schema::run_migrations(&conn)?;
    drop(conn); // 迁移完成后关闭，由连接池管理后续连接

    let manager = SqliteConnectionManager::file(&db_path)
        .with_init(|c| {
            c.pragma_update(None, "journal_mode", "WAL")?;
            c.pragma_update(None, "foreign_keys", "ON")?;
            Ok(())
        });
    let pool = Pool::builder()
        .max_size(8)
        .build(manager)
        .map_err(|e| crate::error::AppError::Other(format!("连接池创建失败: {}", e)))?;

    DB.set(pool).map_err(|_| {
        crate::error::AppError::Other("数据库已初始化，不可重复调用".into())
    })?;
    Ok(())
}

fn with_conn<F, R>(f: F) -> AppResult<R>
where
    F: FnOnce(&rusqlite::Connection) -> AppResult<R>,
{
    let pool = DB.get().ok_or_else(|| {
        crate::error::AppError::Other("数据库未初始化".into())
    })?;
    let conn = pool.get().map_err(|e| {
        crate::error::AppError::Other(format!("获取数据库连接失败: {}", e))
    })?;
    f(&conn)
}

// ============================ Project CRUD ============================

const PROJECT_COLS: &str = "id, name, root_path, java_home, maven_home, env_vars, created_at";

macro_rules! row_to_project {
    ($row:expr) => {
        Project {
            id: $row.get(0)?,
            name: $row.get(1)?,
            root_path: $row.get(2)?,
            java_home: $row.get(3)?,
            maven_home: $row.get(4)?,
            env_vars: $row.get(5)?,
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

pub fn insert_project(name: &str, root_path: &str) -> AppResult<Project> {
    let project = Project {
        id: Uuid::new_v4().to_string(),
        name: name.to_string(),
        root_path: root_path.to_string(),
        java_home: None,
        maven_home: None,
        env_vars: None,
        created_at: Utc::now().to_rfc3339(),
    };
    with_conn(|conn| {
        conn.execute(
            "INSERT INTO projects (id, name, root_path, java_home, maven_home, env_vars, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                project.id, project.name, project.root_path,
                project.java_home, project.maven_home, project.env_vars, project.created_at
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
        // 事务保证原子性：先删服务（及其运行 PID），再删项目
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM service_run_pids WHERE service_id IN (SELECT id FROM services WHERE project_id = ?1)",
            rusqlite::params![id],
        )?;
        tx.execute("DELETE FROM services WHERE project_id = ?1", rusqlite::params![id])?;
        tx.execute("DELETE FROM projects WHERE id = ?1", rusqlite::params![id])?;
        tx.commit()?;
        Ok(())
    })
}

// ============================ Service CRUD ============================

const SERVICE_COLS: &str = "id, name, pom_path, working_dir, project_id, auto_restart, maven_opts, profiles, main_class, dev_mode, override_properties, env_vars, created_at";

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
            main_class: $row.get(8)?,
            dev_mode: $row.get::<_, i64>(9)? != 0,
            override_properties: $row.get(10)?,
            env_vars: $row.get(11)?,
            created_at: $row.get(12)?,
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
    main_class: Option<&str>,
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
        main_class: main_class.map(String::from),
        dev_mode: false,
        override_properties: None,
        env_vars: None,
        created_at: Utc::now().to_rfc3339(),
    };
    with_conn(|conn| {
        conn.execute(
            "INSERT INTO services (id, name, pom_path, working_dir, project_id, auto_restart, maven_opts, profiles, main_class, dev_mode, override_properties, env_vars, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                service.id, service.name, service.pom_path, service.working_dir,
                service.project_id, service.auto_restart as i32,
                service.maven_opts, service.profiles,
                service.main_class, service.dev_mode as i32,
                service.override_properties,
                service.env_vars,
                service.created_at
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
    dev_mode: Option<bool>,
    main_class: Option<Option<&str>>,
    override_properties: Option<Option<&str>>,
    env_vars: Option<Option<&str>>,
) -> AppResult<()> {
    with_conn(|conn| {
        let mut sets: Vec<String> = vec![];
        // 用 rusqlite::types::Value 统一参数类型，避免 Box<dyn ToSql> 的 trait 对象问题
        let mut values: Vec<rusqlite::types::Value> = vec![];

        if let Some(n) = name {
            sets.push("name = ?".to_string());
            values.push(n.to_string().into());
        }
        if let Some(ar) = auto_restart {
            sets.push("auto_restart = ?".to_string());
            values.push((ar as i32).into());
        }
        if let Some(mo) = maven_opts {
            sets.push("maven_opts = ?".to_string());
            values.push(mo.map(|s| s.to_string()).into());
        }
        if let Some(pf) = profiles {
            sets.push("profiles = ?".to_string());
            values.push(pf.map(|s| s.to_string()).into());
        }
        if let Some(dm) = dev_mode {
            sets.push("dev_mode = ?".to_string());
            values.push((dm as i32).into());
        }
        if let Some(mc) = main_class {
            sets.push("main_class = ?".to_string());
            values.push(mc.map(|s| s.to_string()).into());
        }
        if let Some(op) = override_properties {
            sets.push("override_properties = ?".to_string());
            values.push(op.map(|s| s.to_string()).into());
        }
        if let Some(ev) = env_vars {
            sets.push("env_vars = ?".to_string());
            values.push(ev.map(|s| s.to_string()).into());
        }

        if sets.is_empty() {
            return Ok(());
        }

        values.push(id.to_string().into());
        let sql = format!(
            "UPDATE services SET {} WHERE id = ?",
            sets.join(", ")
        );
        let params: Vec<&dyn rusqlite::types::ToSql> = values.iter().map(|v| v as &dyn rusqlite::types::ToSql).collect();
        conn.execute(&sql, params.as_slice())?;
        Ok(())
    })
}

/// 快速写入主类（探测到后内部缓存，失败不阻断启动）
pub fn set_service_main_class(id: &str, main_class: &str) -> AppResult<()> {
    with_conn(|conn| {
        conn.execute(
            "UPDATE services SET main_class = ?1 WHERE id = ?2",
            rusqlite::params![main_class, id],
        )?;
        Ok(())
    })
}

/// 更新项目级 JDK / Maven / 环境变量配置
pub fn update_project_env(
    id: &str,
    java_home: Option<Option<&str>>,
    maven_home: Option<Option<&str>>,
    env_vars: Option<Option<&str>>,
) -> AppResult<()> {
    with_conn(|conn| {
        let tx = conn.unchecked_transaction()?;
        if let Some(jh) = java_home {
            tx.execute(
                "UPDATE projects SET java_home = ?1 WHERE id = ?2",
                rusqlite::params![jh, id],
            )?;
        }
        if let Some(mh) = maven_home {
            tx.execute(
                "UPDATE projects SET maven_home = ?1 WHERE id = ?2",
                rusqlite::params![mh, id],
            )?;
        }
        if let Some(ev) = env_vars {
            tx.execute(
                "UPDATE projects SET env_vars = ?1 WHERE id = ?2",
                rusqlite::params![ev, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    })
}

pub fn delete_service(id: &str) -> AppResult<()> {
    // 确保外键约束生效
    with_conn(|conn| {
        let tx = conn.unchecked_transaction()?;
        tx.execute("DELETE FROM services WHERE id = ?1", rusqlite::params![id])?;
        tx.execute(
            "DELETE FROM service_run_pids WHERE service_id = ?1",
            rusqlite::params![id],
        )?;
        // service_dependencies 的 FK 是 ON DELETE CASCADE，但 SQLite 的
        // unchecked_transaction 下 PRAGMA foreign_keys=ON 已在连接初始化时设置，
        // 所以服务删除后依赖行会自动清理。这里显式删一次更保险。
        tx.execute(
            "DELETE FROM service_dependencies WHERE service_id = ?1 OR depends_on = ?1",
            rusqlite::params![id],
        )?;
        tx.commit()?;
        Ok(())
    })
}

// ============================ Service Dependencies ============================

/// 查询某个服务的直接依赖列表（depends_on IDs）
pub fn list_dependencies(service_id: &str) -> AppResult<Vec<String>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT depends_on FROM service_dependencies WHERE service_id = ?1 ORDER BY depends_on",
        )?;
        let rows = stmt.query_map(rusqlite::params![service_id], |row| {
            row.get::<_, String>(0)
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

/// 查询全部依赖关系
pub fn list_all_dependencies() -> AppResult<Vec<Dependency>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT service_id, depends_on FROM service_dependencies",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Dependency {
                service_id: row.get(0)?,
                depends_on: row.get(1)?,
            })
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

/// 全量替换某个服务的依赖列表（先删后插，事务保护）
pub fn set_dependencies(service_id: &str, depends_on_ids: &[String]) -> AppResult<()> {
    with_conn(|conn| {
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM service_dependencies WHERE service_id = ?1",
            rusqlite::params![service_id],
        )?;
        for dep in depends_on_ids {
            // 不允许自引用
            if dep == service_id {
                continue;
            }
            tx.execute(
                "INSERT OR IGNORE INTO service_dependencies (service_id, depends_on) VALUES (?1, ?2)",
                rusqlite::params![service_id, dep],
            )?;
        }
        tx.commit()?;
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
                "dev_lazy_init" => cfg.dev_lazy_init = v == "true",
                _ => {}
            }
        }
        Ok(())
    })?;
    Ok(cfg)
}

pub fn save_config(cfg: &AppConfig) -> AppResult<()> {
    with_conn(|conn| {
        let tx = conn.unchecked_transaction()?;
        let pairs = [
            ("port_refresh_interval_secs", cfg.port_refresh_interval_secs.to_string()),
            ("stop_on_compile_fail", cfg.stop_on_compile_fail.to_string()),
            ("auto_restart_debounce_secs", cfg.auto_restart_debounce_secs.to_string()),
            ("log_buffer_lines", cfg.log_buffer_lines.to_string()),
            ("stop_all_on_exit", cfg.stop_all_on_exit.to_string()),
            ("dev_lazy_init", cfg.dev_lazy_init.to_string()),
        ];
        for (k, v) in pairs {
            tx.execute(
                "INSERT INTO app_config (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
                rusqlite::params![k, v],
            )?;
        }
        tx.commit()?;
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
