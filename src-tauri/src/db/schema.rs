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

/// 增量迁移：projects 加 JDK/Maven 列，services 保留 maven_opts/profiles
fn migrate_v2(conn: &Connection) -> rusqlite::Result<()> {
    let add_column = |table: &str, col: &str, ty: &str| -> rusqlite::Result<()> {
        let exists: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM pragma_table_info('{}') WHERE name = ?1", table),
            rusqlite::params![col],
            |r| r.get(0),
        )?;
        if exists == 0 {
            conn.execute_batch(&format!("ALTER TABLE {} ADD COLUMN {} {}", table, col, ty))?;
        }
        Ok(())
    };
    // projects: 加 java_home / maven_home（项目级）
    add_column("projects", "java_home", "TEXT")?;
    add_column("projects", "maven_home", "TEXT")?;
    // services: 加 maven_opts / profiles（服务级）
    add_column("services", "maven_opts", "TEXT")?;
    add_column("services", "profiles", "TEXT")?;
    Ok(())
}

pub fn run_migrations(conn: &Connection) -> rusqlite::Result<()> {
    for sql in MIGRATIONS {
        conn.execute_batch(sql)?;
    }
    migrate_v2(conn)?;
    // seed default config
    let defaults = [
        ("port_refresh_interval_secs", "2"),
        ("stop_on_compile_fail", "false"),
        ("auto_restart_debounce_secs", "3"),
        ("log_buffer_lines", "10000"),
        ("stop_all_on_exit", "true"),
    ];
    for (k, v) in defaults {
        conn.execute(
            "INSERT OR IGNORE INTO app_config (key, value) VALUES (?1, ?2)",
            rusqlite::params![k, v],
        )?;
    }
    Ok(())
}
