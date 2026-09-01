use rusqlite::Connection;

pub const MIGRATIONS: &[&str] = &[
    // v1 init
    r#"
    CREATE TABLE IF NOT EXISTS projects (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        root_path TEXT NOT NULL,
        git_available INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS services (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        pom_path TEXT NOT NULL,
        working_dir TEXT NOT NULL,
        project_id TEXT,
        auto_restart INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL,
        FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE SET NULL
    );

    CREATE INDEX IF NOT EXISTS idx_services_project ON services(project_id);

    CREATE TABLE IF NOT EXISTS app_config (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS service_run_pids (
        service_id TEXT PRIMARY KEY,
        pid INTEGER NOT NULL,
        started_at TEXT NOT NULL
    );
    "#,
];

/// 通用工具：安全 ADD COLUMN（幂等）
fn add_column(conn: &Connection, table: &str, col: &str, ty: &str) -> rusqlite::Result<()> {
    let exists: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM pragma_table_info('{}') WHERE name = ?1", table),
        rusqlite::params![col],
        |r| r.get(0),
    )?;
    if exists == 0 {
        conn.execute_batch(&format!("ALTER TABLE {} ADD COLUMN {} {}", table, col, ty))?;
    }
    Ok(())
}

/// 增量迁移：projects 加 JDK/Maven 列，services 保留 maven_opts/profiles
fn migrate_v2(conn: &Connection) -> rusqlite::Result<()> {
    // projects: 加 java_home / maven_home（项目级）
    add_column(conn, "projects", "java_home", "TEXT")?;
    add_column(conn, "projects", "maven_home", "TEXT")?;
    // services: 加 maven_opts / profiles（服务级）
    add_column(conn, "services", "maven_opts", "TEXT")?;
    add_column(conn, "services", "profiles", "TEXT")?;
    Ok(())
}

/// v3：services 增加 main_class（缓存主类）与 dev_mode（快速启动开关）
fn migrate_v3(conn: &Connection) -> rusqlite::Result<()> {
    add_column(conn, "services", "main_class", "TEXT")?;
    add_column(conn, "services", "dev_mode", "INTEGER NOT NULL DEFAULT 0")?;
    Ok(())
}

/// v4：services 增加 override_properties（JSON 数组，存 -D 覆盖属性 key/value）
fn migrate_v4(conn: &Connection) -> rusqlite::Result<()> {
    add_column(conn, "services", "override_properties", "TEXT")?;
    Ok(())
}

/// v5：服务依赖编排表（多对多，表示 service_id 依赖 depends_on 先启动）
fn migrate_v5(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS service_dependencies (
            service_id  TEXT NOT NULL,
            depends_on  TEXT NOT NULL,
            PRIMARY KEY (service_id, depends_on),
            FOREIGN KEY (service_id) REFERENCES services(id) ON DELETE CASCADE,
            FOREIGN KEY (depends_on) REFERENCES services(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_deps_depends_on ON service_dependencies(depends_on);
        "#,
    )?;
    Ok(())
}

/// v6：项目级 & 服务级自定义环境变量（JSON 数组 `[{"key":"K","value":"V"}]`）
///
/// 项目级对该项目下所有服务生效；服务级同名 key 覆盖项目级。
/// 启动时由 `inject_env` 注入到 mvn 编译进程与 java 运行进程。
fn migrate_v6(conn: &Connection) -> rusqlite::Result<()> {
    add_column(conn, "projects", "env_vars", "TEXT")?;
    add_column(conn, "services", "env_vars", "TEXT")?;
    Ok(())
}

pub fn run_migrations(conn: &Connection) -> rusqlite::Result<()> {
    // 当前迁移版本号：每次新增迁移时递增此值并添加对应的 migrate_vN 函数
    const CURRENT_VERSION: u32 = 6;

    // 读取已执行的迁移版本
    let user_version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap_or(0);

    // v1 基础表（仅在 user_version < 1 时执行）
    if user_version < 1 {
        for sql in MIGRATIONS {
            conn.execute_batch(sql)?;
        }
    }
    // v2-v6 增量迁移：仅执行未执行的版本
    if user_version < 2 {
        migrate_v2(conn)?;
    }
    if user_version < 3 {
        migrate_v3(conn)?;
    }
    if user_version < 4 {
        migrate_v4(conn)?;
    }
    if user_version < 5 {
        migrate_v5(conn)?;
    }
    if user_version < 6 {
        migrate_v6(conn)?;
    }

    // 更新版本号
    conn.execute_batch(&format!("PRAGMA user_version = {}", CURRENT_VERSION))?;

    // seed default config（幂等，已有 key 不会覆盖）
    let defaults = [
        ("port_refresh_interval_secs", "2"),
        ("stop_on_compile_fail", "false"),
        ("auto_restart_debounce_secs", "3"),
        ("log_buffer_lines", "10000"),
        ("stop_all_on_exit", "true"),
        ("dev_lazy_init", "false"),
    ];
    for (k, v) in defaults {
        conn.execute(
            "INSERT OR IGNORE INTO app_config (key, value) VALUES (?1, ?2)",
            rusqlite::params![k, v],
        )?;
    }
    Ok(())
}
