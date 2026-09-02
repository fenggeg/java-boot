//! 运行事实存储：SQLite(WAL) 三表。
//!
//! 表结构与 ADR-0001 决策 3 完全一致。同步内层用阻塞 `rusqlite`，所有公开的异步
//! 方法通过 `spawn_blocking` 将任务委托到阻塞线程，绝不在 async 上下文直接做阻塞 IO。
//!
//! 线程安全说明：`rusqlite::Connection` 是 `Send` 而非 `Sync`，故用
//! `parking_lot::Mutex<Connection>` 包住，使 `Store: Send + Sync`，
//! 可整体放进 `Arc` 供各服务共享。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};

use jb_core::model::{LogLine, ProcessSpec, Stream};
use tokio::task::spawn_blocking;

use crate::error::{Error, Result};

/// 默认数据目录下的 DB 文件名。
pub const DB_FILE: &str = "daemon.db";

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// 打开（或创建）数据库并建表。返回 `Arc<Store>`。
    pub fn open(db_path: &Path) -> Result<Arc<Store>> {
        if let Some(dir) = db_path.parent() {
            std::fs::create_dir_all(dir).map_err(Error::Io)?;
        }
        let conn = Connection::open(db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;",
        )?;
        conn.execute_batch(SCHEMA)?;
        Ok(Arc::new(Store { conn: Mutex::new(conn) }))
    }

    /// daemon 默认数据目录：`%LOCALAPPDATA%\javaboot-daemon`。
    pub fn default_dir() -> PathBuf {
        dirs::data_local_dir()
            .or_else(dirs::data_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("javaboot-daemon")
    }

    /// 默认 DB 完整路径。
    pub fn default_db_path() -> PathBuf {
        Self::default_dir().join(DB_FILE)
    }
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS process_spec (
    run_id INTEGER PRIMARY KEY,
    project_id TEXT NOT NULL,
    module_name TEXT NOT NULL,
    main_class TEXT,
    classpath_key TEXT,
    jvm_args TEXT,
    env_vars TEXT,
    working_dir TEXT,
    dev_mode INTEGER,
    auto_restart INTEGER,
    log_file TEXT,
    launcher_version TEXT,
    startup_port INTEGER,
    created_at INTEGER
);

CREATE TABLE IF NOT EXISTS service_run (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT,
    module_name TEXT,
    pid INTEGER,
    started_at INTEGER,
    exit_code INTEGER,
    exit_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_run_module ON service_run(project_id, module_name);

CREATE TABLE IF NOT EXISTS service_log (
    run_id INTEGER NOT NULL,
    seq INTEGER NOT NULL,
    ts INTEGER NOT NULL,
    stream TEXT NOT NULL,
    level TEXT,
    body TEXT NOT NULL,
    PRIMARY KEY (run_id, seq)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS scan_cache (
    project_key TEXT PRIMARY KEY,
    modules_json TEXT NOT NULL,
    scanned_at INTEGER NOT NULL
);
"#;

// ---------------------------------------------------------------------------
// 同步内层（须在 spawn_blocking 线程内调用）
// ---------------------------------------------------------------------------
impl Store {
    fn _insert_run(&self, project_id: &str, module_name: &str) -> Result<i64> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO service_run (project_id, module_name, started_at) VALUES (?1, ?2, ?3)",
            params![project_id, module_name, jb_core::model::now_ms()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    fn _insert_spec(&self, spec: &ProcessSpec) -> Result<()> {
        self.conn.lock().execute(
            "INSERT OR REPLACE INTO process_spec (
                run_id, project_id, module_name, main_class, classpath_key,
                jvm_args, env_vars, working_dir, dev_mode, auto_restart,
                log_file, launcher_version, startup_port, created_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![
                spec.run_id,
                spec.project_id,
                spec.module_name,
                spec.main_class,
                spec.classpath_key,
                spec.jvm_args,
                spec.env_vars,
                spec.working_dir,
                i64::from(spec.dev_mode),
                i64::from(spec.auto_restart),
                spec.log_file,
                spec.launcher_version,
                spec.startup_port,
                spec.created_at,
            ],
        )?;
        Ok(())
    }

    fn _set_run_pid(&self, run_id: i64, pid: u32) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE service_run SET pid = ?1 WHERE id = ?2",
            params![i64::from(pid), run_id],
        )?;
        Ok(())
    }

    fn _finish_run(&self, run_id: i64, exit_code: i32) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE service_run SET exit_code = ?1, exit_at = ?2
             WHERE id = ?3 AND exit_at IS NULL",
            params![exit_code, jb_core::model::now_ms(), run_id],
        )?;
        Ok(())
    }

    fn _assign_log_file(&self, run_id: i64, log_file: &str) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE process_spec SET log_file = ?1 WHERE run_id = ?2",
            params![log_file, run_id],
        )?;
        Ok(())
    }

    fn _write_logs(&self, lines: &[LogLine]) -> Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock();
        let mut stmt = conn.prepare_cached(
            "INSERT OR IGNORE INTO service_log (run_id, seq, ts, stream, level, body)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for l in lines {
            stmt.execute(params![
                l.run_id,
                l.seq,
                l.ts,
                stream_str(l.stream),
                l.level,
                l.body,
            ])?;
        }
        Ok(())
    }

    fn _tail(&self, run_id: i64, after_seq: i64, limit: usize) -> Result<(i64, Vec<LogLine>)> {
        let cap = lim(limit, 5000);
        let conn = self.conn.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT run_id, seq, ts, stream, level, body
             FROM service_log WHERE run_id = ?1 AND seq > ?2
             ORDER BY seq ASC LIMIT ?3",
        )?;
        let mut rows = stmt.query(params![run_id, after_seq, cap as i64])?;
        let mut entries = Vec::with_capacity(cap as usize);
        while let Some(r) = rows.next()? {
            let run: i64 = r.get(0)?;
            let seq: i64 = r.get(1)?;
            let ts: i64 = r.get(2)?;
            let stream_raw: String = r.get(3)?;
            let level: Option<String> = r.get(4)?;
            let body: String = r.get(5)?;
            entries.push(LogLine {
                run_id: run,
                seq,
                ts,
                stream: parse_stream(&stream_raw),
                level,
                body,
            });
        }
        let next_seq = entries.last().map(|l| l.seq).unwrap_or(after_seq);
        Ok((next_seq, entries))
    }

    fn _get_spec(&self, run_id: i64) -> Result<Option<ProcessSpec>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT run_id, project_id, module_name, main_class, classpath_key,
                    jvm_args, env_vars, working_dir, dev_mode, auto_restart,
                    log_file, launcher_version, startup_port, created_at
             FROM process_spec WHERE run_id = ?1",
        )?;
        let mut rows = stmt.query_map(params![run_id], |r| {
            Ok(ProcessSpec {
                run_id: r.get(0)?,
                project_id: r.get(1)?,
                module_name: r.get(2)?,
                main_class: r.get(3)?,
                classpath_key: r.get(4)?,
                jvm_args: r.get(5)?,
                env_vars: r.get(6)?,
                working_dir: r.get(7)?,
                dev_mode: r.get::<_, i64>(8)? != 0,
                auto_restart: r.get::<_, i64>(9)? != 0,
                log_file: r.get(10)?,
                launcher_version: r.get(11)?,
                startup_port: r.get(12)?,
                created_at: r.get(13)?,
            })
        })?;
        rows.next().transpose().map_err(Error::from)
    }

    fn _run_count_active(&self) -> Result<i64> {
        let n: i64 = self.conn.lock().query_row(
            "SELECT COUNT(*) FROM service_run WHERE exit_at IS NULL",
            [],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    fn _cleanup(&self, retention_days: i64) -> Result<()> {
        let cutoff_ms = jb_core::model::now_ms() - retention_days * 86_400_000;
        let conn = self.conn.lock();
        conn.execute(
            "DELETE FROM service_log WHERE ts < ?1",
            params![cutoff_ms],
        )?;
        conn.execute(
            "DELETE FROM service_run WHERE exit_at IS NOT NULL AND exit_at < ?1",
            params![cutoff_ms],
        )?;
        // 孤儿 spec（对应 run 已被删除）
        conn.execute(
            "DELETE FROM process_spec WHERE run_id NOT IN (SELECT id FROM service_run)",
            [],
        )?;
        Ok(())
    }

    fn _list_specs_all(&self) -> Result<Vec<ProcessSpec>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT run_id, project_id, module_name, main_class, classpath_key,
                    jvm_args, env_vars, working_dir, dev_mode, auto_restart,
                    log_file, launcher_version, startup_port, created_at
             FROM process_spec",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ProcessSpec {
                run_id: r.get(0)?,
                project_id: r.get(1)?,
                module_name: r.get(2)?,
                main_class: r.get(3)?,
                classpath_key: r.get(4)?,
                jvm_args: r.get(5)?,
                env_vars: r.get(6)?,
                working_dir: r.get(7)?,
                dev_mode: r.get::<_, i64>(8)? != 0,
                auto_restart: r.get::<_, i64>(9)? != 0,
                log_file: r.get(10)?,
                launcher_version: r.get(11)?,
                startup_port: r.get(12)?,
                created_at: r.get(13)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Error::from)
    }

    fn _list_run_pids(&self) -> Result<Vec<(i64, Option<u32>)>> {
        let conn = self.conn.lock();
        let mut stmt =
            conn.prepare_cached("SELECT id, pid FROM service_run WHERE exit_at IS NULL")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<i32>>(1)?.map(|p| p as u32),
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Error::from)
    }

    fn _get_scan_cache(&self, project_key: &str) -> Result<Option<(String, i64)>> {
        self.conn
            .lock()
            .query_row(
                "SELECT modules_json, scanned_at FROM scan_cache WHERE project_key = ?1",
                params![project_key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(Error::from)
    }

    fn _set_scan_cache(&self, project_key: &str, modules_json: &str) -> Result<()> {
        self.conn.lock().execute(
            "INSERT OR REPLACE INTO scan_cache (project_key, modules_json, scanned_at) VALUES (?1, ?2, ?3)",
            params![project_key, modules_json, jb_core::model::now_ms()],
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 异步公开接口：clone Arc 后经 spawn_blocking 委托同步内层
// ---------------------------------------------------------------------------
impl Store {
    pub async fn insert_run(self: Arc<Self>, project_id: String, module_name: String) -> Result<i64> {
        spawn_blocking(move || self._insert_run(&project_id, &module_name))
            .await
            .map_err(|e| Error::Other(format!("插入 run 任务失败: {e}")))?
    }

    pub async fn insert_spec(self: Arc<Self>, spec: ProcessSpec) -> Result<()> {
        spawn_blocking(move || self._insert_spec(&spec))
            .await
            .map_err(|e| Error::Other(format!("写 spec 任务失败: {e}")))?
    }

    pub async fn set_run_pid(self: Arc<Self>, run_id: i64, pid: u32) -> Result<()> {
        spawn_blocking(move || self._set_run_pid(run_id, pid))
            .await
            .map_err(|e| Error::Other(format!("写 PID 任务失败: {e}")))?
    }

    pub async fn finish_run(self: Arc<Self>, run_id: i64, exit_code: i32) -> Result<()> {
        spawn_blocking(move || self._finish_run(run_id, exit_code))
            .await
            .map_err(|e| Error::Other(format!("结束 run 任务失败: {e}")))?
    }

    pub async fn assign_log_file(self: Arc<Self>, run_id: i64, log_file: String) -> Result<()> {
        spawn_blocking(move || self._assign_log_file(run_id, &log_file))
            .await
            .map_err(|e| Error::Other(format!("写日志文件任务失败: {e}")))?
    }

    pub async fn write_logs(self: Arc<Self>, lines: Vec<LogLine>) -> Result<()> {
        spawn_blocking(move || self._write_logs(&lines))
            .await
            .map_err(|e| Error::Other(format!("批量写日志任务失败: {e}")))?
    }

    pub async fn tail(self: Arc<Self>, run_id: i64, after_seq: i64, limit: usize) -> Result<(i64, Vec<LogLine>)> {
        spawn_blocking(move || self._tail(run_id, after_seq, limit))
            .await
            .map_err(|e| Error::Other(format!("拉取日志任务失败: {e}")))?
    }

    pub async fn get_spec(self: Arc<Self>, run_id: i64) -> Result<Option<ProcessSpec>> {
        spawn_blocking(move || self._get_spec(run_id))
            .await
            .map_err(|e| Error::Other(format!("读取 spec 任务失败: {e}")))?
    }

    pub async fn run_count_active(self: Arc<Self>) -> Result<i64> {
        spawn_blocking(move || self._run_count_active())
            .await
            .map_err(|e| Error::Other(format!("统计 run 任务失败: {e}")))?
    }

    pub async fn cleanup(self: Arc<Self>, retention_days: i64) -> Result<()> {
        spawn_blocking(move || self._cleanup(retention_days))
            .await
            .map_err(|e| Error::Other(format!("清理任务失败: {e}")))?
    }

    pub async fn list_specs_all(self: Arc<Self>) -> Result<Vec<ProcessSpec>> {
        spawn_blocking(move || self._list_specs_all())
            .await
            .map_err(|e| Error::Other(format!("列出 spec 任务失败: {e}")))?
    }

    pub async fn list_run_pids(self: Arc<Self>) -> Result<Vec<(i64, Option<u32>)>> {
        spawn_blocking(move || self._list_run_pids())
            .await
            .map_err(|e| Error::Other(format!("列出 run 任务失败: {e}")))?
    }

    pub async fn get_scan_cache(self: Arc<Self>, project_key: String) -> Result<Option<(String, i64)>> {
        spawn_blocking(move || self._get_scan_cache(&project_key))
            .await
            .map_err(|e| Error::Other(format!("读扫描缓存失败: {e}")))?
    }

    pub async fn set_scan_cache(self: Arc<Self>, project_key: String, modules_json: String) -> Result<()> {
        spawn_blocking(move || self._set_scan_cache(&project_key, &modules_json))
            .await
            .map_err(|e| Error::Other(format!("写扫描缓存失败: {e}")))?
    }
}

/// 保留首尾策略用到的截取：把超限 run 的日志压缩成 `[TRUNCATED]`（后继 P0 定时任务用到）。
#[allow(dead_code)]
fn truncate_logs(_lines: &mut Vec<LogLine>, _keep: i64) {
    // 预留：单 run > 50MB 时保留首尾 5MB，中间替换为 `[TRUNCATED]`。
    // flusher 在写库前调用。
}

fn lim(v: usize, max: usize) -> usize {
    v.min(max)
}

fn stream_str(s: Stream) -> &'static str {
    match s {
        Stream::Stdout => "stdout",
        Stream::Stderr => "stderr",
    }
}

fn parse_stream(s: &str) -> Stream {
    if s.eq_ignore_ascii_case("stderr") {
        Stream::Stderr
    } else {
        Stream::Stdout
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store() -> Arc<Store> {
        let dir = std::env::temp_dir().join(format!("jb-store-test-{}", uuid::Uuid::new_v4()));
        Store::open(&dir.join(DB_FILE)).unwrap()
    }

    #[tokio::test]
    async fn insert_run_and_spec_roundtrip() {
        let s = tmp_store();
        let run_id = s.clone().insert_run("proj".into(), "mod".into()).await.unwrap();
        let spec = ProcessSpec {
            run_id,
            project_id: "proj".into(),
            module_name: "mod".into(),
            main_class: Some("com.Main".into()),
            classpath_key: None,
            jvm_args: r#"["java","-cp","."]"#.into(),
            env_vars: r#"{"DB_PASSWORD":"«redacted»","PORT":"8080"}"#.into(),
            working_dir: r"C:\work".into(),
            dev_mode: false,
            auto_restart: true,
            log_file: String::new(),
            launcher_version: "0.16.0".into(),
            startup_port: Some(8080),
            created_at: 1,
        };
        s.clone().insert_spec(spec).await.unwrap();
        let got = s.clone().get_spec(run_id).await.unwrap().unwrap();
        assert_eq!(got.main_class.as_deref(), Some("com.Main"));
        assert_eq!(got.startup_port, Some(8080));
        assert!(got.env_vars.contains("«redacted»"));
    }

    #[tokio::test]
    async fn batch_write_logs_then_tail() {
        let s = tmp_store();
        let run_id = s.clone().insert_run("p".into(), "m".into()).await.unwrap();
        let lines: Vec<LogLine> = (0..30)
            .map(|i| LogLine {
                run_id,
                seq: i as i64 + 1,
                ts: i,
                stream: if i % 2 == 0 { Stream::Stdout } else { Stream::Stderr },
                level: None,
                body: format!("line {i}"),
            })
            .collect();
        s.clone().write_logs(lines.clone()).await.unwrap();
        let (next, got) = s.clone().tail(run_id, 0, 100).await.unwrap();
        assert_eq!(got.len(), 30);
        assert_eq!(next, 30);
        assert_eq!(got.first().unwrap().body, "line 0");
        // 增量拉取
        let (_, delta) = s.clone().tail(run_id, 20, 100).await.unwrap();
        assert_eq!(delta.len(), 10);
    }

    #[tokio::test]
    async fn finish_run_sets_exit() {
        let s = tmp_store();
        let run_id = s.clone().insert_run("p".into(), "m".into()).await.unwrap();
        s.clone().set_run_pid(run_id, 1234).await.unwrap();
        s.clone().finish_run(run_id, 0).await.unwrap();
        assert_eq!(s.clone().run_count_active().await.unwrap(), 0);
        // 幂等：重复 finish 不报错
        s.clone().finish_run(run_id, 1).await.unwrap();
    }
}