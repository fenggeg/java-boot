//! 进程管理器（薄壳版）
//!
//! 关键子模块：
//! - [`super::log_pipe`]：日志推送 & 启动/失败检测
//! - [`super::env`]：环境解析 & 命令定位（含 PATH 探测缓存）
//! - [`super::build`]：主类探测 / classpath 缓存 / mtime 决策 / mvn 执行器

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::Utc;
use once_cell::sync::Lazy;
use parking_lot::Mutex as PMutex;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::db;
use crate::db::models::{Service, ServiceRuntime, ServiceStatus};
use crate::error::{AppError, AppResult};

use super::build::{
    self, collect_sibling_classes, common_mvn_flags, decide_build_strategy, detect_main_class,
    run_mvn_capture, BuildStrategy, ClasspathCache, CompilePidSlot,
};
use super::env::{
    inject_env, preflight_check, resolve_env_config, resolve_java_home, resolve_maven_cmd,
    EnvConfig,
};
use super::job::JobObject;
use super::log_pipe::{check_failed, check_started, emit_log_raw, LogSource};

// ================================================================
// ProcessHandle
// ================================================================

struct ProcessHandle {
    /// Java 进程 PID（spawn 之前为 0）
    pid: u32,
    /// Java 进程的 Job Object（spawn 之前为 None）
    job: Option<Arc<PMutex<JobObject>>>,
    /// 取消令牌：换成 AtomicBool，读写无锁
    kill_token: Arc<AtomicBool>,
    /// 编译期子进程 PID（用于 stop 中断编译）
    compile_pid: CompilePidSlot,
}

impl ProcessHandle {
    fn placeholder() -> Self {
        Self {
            pid: 0,
            job: None,
            kill_token: Arc::new(AtomicBool::new(false)),
            compile_pid: Arc::new(PMutex::new(None)),
        }
    }
}

/// 按 PID 杀掉整个进程树（用于恢复后无 Job Object 的服务）
fn kill_process_tree_by_pid(pid: u32) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(not(windows))]
    {
        let mut sys = sysinfo::System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, false);
        if let Some(p) = sys.process(sysinfo::Pid::from_u32(pid)) {
            p.kill();
        }
        let _ = pid;
    }
}

// ================================================================
// ProcessManager
// ================================================================

pub struct ProcessManager {
    handles: PMutex<HashMap<String, ProcessHandle>>,
    runtimes: PMutex<HashMap<String, ServiceRuntime>>,
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            handles: PMutex::new(HashMap::new()),
            runtimes: PMutex::new(HashMap::new()),
        }
    }

    pub fn get_runtime(&self, service_id: &str) -> ServiceRuntime {
        self.runtimes
            .lock()
            .get(service_id)
            .cloned()
            .unwrap_or_else(|| {
                let mut r = ServiceRuntime::default();
                r.service_id = service_id.to_string();
                r
            })
    }

    pub fn all_runtimes(&self) -> Vec<ServiceRuntime> {
        self.runtimes.lock().values().cloned().collect()
    }

    /// 应用启动时恢复：检查持久化的 PID 是否还活着
    pub fn restore_running_services(&self, app: &AppHandle) {
        let pids = match db::load_all_run_pids() {
            Ok(p) => p,
            Err(_) => return,
        };
        if pids.is_empty() {
            return;
        }

        let pid_refs: Vec<sysinfo::Pid> =
            pids.iter().map(|(_, pid, _)| sysinfo::Pid::from_u32(*pid)).collect();
        {
            let mut sys = SYS.lock();
            sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&pid_refs), false);
            for (service_id, pid, started_at) in &pids {
                if sys.process(sysinfo::Pid::from_u32(*pid)).is_some() {
                    log::info!("恢复服务 {} (PID {})", service_id, pid);
                    let mut rt = self.runtimes.lock();
                    let entry = rt.entry(service_id.clone()).or_default();
                    entry.service_id = service_id.clone();
                    entry.status = ServiceStatus::Running;
                    entry.pid = Some(*pid);
                    entry.started_at = Some(started_at.clone());
                    entry.ports = crate::port::ports_for_pid(*pid).unwrap_or_default();
                } else {
                    let _ = db::clear_run_pid(service_id);
                    log::info!("服务 {} 的进程已不存在，清理", service_id);
                }
            }
        }

        for rt in self.all_runtimes() {
            let _ = app.emit("service://status", rt);
        }
    }

    /// 刷新所有运行中服务的 CPU/内存占用
    pub fn refresh_resource_usage(&self, app: &AppHandle) {
        let pids: Vec<(String, u32)> = {
            let rt = self.runtimes.lock();
            rt.values()
                .filter(|r| r.status == ServiceStatus::Running)
                .filter_map(|r| r.pid.map(|p| (r.service_id.clone(), p)))
                .collect()
        };
        if pids.is_empty() {
            return;
        }
        let pid_refs: Vec<sysinfo::Pid> =
            pids.iter().map(|(_, p)| sysinfo::Pid::from_u32(*p)).collect();
        {
            let mut sys = SYS.lock();
            sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&pid_refs), true);
            let mut rt = self.runtimes.lock();
            for (service_id, pid) in &pids {
                if let Some(proc) = sys.process(sysinfo::Pid::from_u32(*pid)) {
                    let entry = rt.entry(service_id.clone()).or_default();
                    entry.service_id = service_id.clone();
                    entry.cpu_usage = Some(proc.cpu_usage());
                    entry.memory_mb = Some(proc.memory() as f64 / 1024.0 / 1024.0);
                }
            }
        }
        for rt in self.all_runtimes() {
            let _ = app.emit("service://status", rt);
        }
    }

    /// 集中刷新监听端口（单次全表扫描，按 PID 归属分发）
    pub fn refresh_ports(&self, app: &AppHandle) {
        let service_pids: Vec<(String, u32)> = {
            let rt = self.runtimes.lock();
            rt.values()
                .filter(|r| r.status == ServiceStatus::Running)
                .filter_map(|r| r.pid.map(|p| (r.service_id.clone(), p)))
                .collect()
        };
        if service_pids.is_empty() {
            return;
        }
        let table = match crate::port::all_listening_ports() {
            Ok(t) => t,
            Err(_) => return,
        };
        let mut pid_ports: HashMap<u32, Vec<u16>> = HashMap::new();
        for (port, owner_pid) in &table {
            pid_ports.entry(*owner_pid).or_default().push(*port);
        }
        let mut changed = false;
        {
            let mut rt = self.runtimes.lock();
            for (service_id, pid) in &service_pids {
                let ports = pid_ports.get(pid).cloned().unwrap_or_default();
                let entry = rt.entry(service_id.clone()).or_default();
                entry.service_id = service_id.clone();
                if entry.ports != ports {
                    entry.ports = ports;
                    changed = true;
                }
            }
        }
        if changed {
            self.refresh_port_conflicts(app);
            for rt in self.all_runtimes() {
                let _ = app.emit("service://status", rt);
            }
        }
    }

    pub fn set_status(&self, app: &AppHandle, service_id: &str, status: ServiceStatus) {
        let mut rt = self.runtimes.lock();
        let entry = rt.entry(service_id.to_string()).or_default();
        entry.service_id = service_id.to_string();
        entry.status = status;
        if status == ServiceStatus::Stopped || status == ServiceStatus::Error {
            entry.pid = None;
            entry.ports.clear();
            entry.port_conflict = false;
            entry.conflict_with.clear();
            if status == ServiceStatus::Stopped {
                entry.started_at = None;
            }
        }
        let snapshot = entry.clone();
        drop(rt);
        let _ = app.emit("service://status", snapshot);
    }

    fn set_pid(&self, app: &AppHandle, service_id: &str, pid: u32) {
        let mut rt = self.runtimes.lock();
        let entry = rt.entry(service_id.to_string()).or_default();
        entry.service_id = service_id.to_string();
        entry.pid = Some(pid);
        entry.started_at = Some(Utc::now().to_rfc3339());
        let snapshot = entry.clone();
        drop(rt);
        let _ = app.emit("service://status", snapshot);
        let _ = db::save_run_pid(service_id, pid);
    }

    /// 标记端口冲突
    pub fn refresh_port_conflicts(&self, app: &AppHandle) {
        let mut rt = self.runtimes.lock();
        let mut port_owners: HashMap<u16, Vec<String>> = HashMap::new();
        for r in rt.values() {
            for p in &r.ports {
                port_owners.entry(*p).or_default().push(r.service_id.clone());
            }
        }
        for r in rt.values_mut() {
            let mut conflicts: Vec<String> = vec![];
            for p in &r.ports {
                if let Some(owners) = port_owners.get(p) {
                    for o in owners {
                        if o != &r.service_id && !conflicts.contains(o) {
                            conflicts.push(o.clone());
                        }
                    }
                }
            }
            r.port_conflict = !conflicts.is_empty();
            r.conflict_with = conflicts;
        }
        let snapshots: Vec<ServiceRuntime> = rt.values().cloned().collect();
        drop(rt);
        for s in snapshots {
            let _ = app.emit("service://status", s);
        }
    }

    fn emit_log(app: &AppHandle, service_id: &str, source: LogSource, line: &str) {
        emit_log_raw(app, service_id, source.tag(), line);
    }

    /// 公开静态方法：供其他模块（如 git）推送日志
    pub fn emit_log_static(app: &AppHandle, service_id: &str, tag: &str, line: &str) {
        emit_log_raw(app, service_id, tag, line);
    }

    pub fn is_running(&self, service_id: &str) -> bool {
        if self.handles.lock().contains_key(service_id) {
            return true;
        }
        matches!(
            self.get_runtime(service_id).status,
            ServiceStatus::Running | ServiceStatus::Starting | ServiceStatus::Recompiling
        )
    }

    fn mark_running(&self, app: &AppHandle, service_id: &str) {
        self.set_status(app, service_id, ServiceStatus::Running);
    }

    // ================================================================
    // start：核心启动流程（三档策略 + classpath 缓存 + dev_mode）
    // ================================================================

    pub async fn start(&self, app: AppHandle, service: Service) -> AppResult<()> {
        let (kill_token, compile_pid) = {
            let mut handles = self.handles.lock();
            if handles.contains_key(&service.id) {
                return Err(AppError::ServiceRunning(service.id));
            }
            let placeholder = ProcessHandle::placeholder();
            let kt = placeholder.kill_token.clone();
            let cp = placeholder.compile_pid.clone();
            handles.insert(service.id.clone(), placeholder);
            (kt, cp)
        };

        self.set_status(&app, &service.id, ServiceStatus::Starting);

        macro_rules! check_cancel {
            () => {
                if kill_token.load(Ordering::Relaxed) {
                    self.handles.lock().remove(&service.id);
                    self.set_status(&app, &service.id, ServiceStatus::Stopped);
                    Self::emit_log(&app, &service.id, LogSource::Mvn, "[javaboot] 启动已取消");
                    return Ok(());
                }
            };
        }
        macro_rules! try_cleanup {
            ($e:expr) => {
                match $e {
                    Ok(v) => v,
                    Err(e) => {
                        self.handles.lock().remove(&service.id);
                        self.set_status(&app, &service.id, ServiceStatus::Stopped);
                        return Err(e.into());
                    }
                }
            };
        }

        let working_dir = PathBuf::from(&service.working_dir);
        let env_cfg = try_cleanup!(resolve_env_config(&service));
        let (program, base_args) = resolve_maven_cmd(&working_dir, &env_cfg);
        try_cleanup!(preflight_check(&env_cfg, &working_dir, &program));

        let cache = ClasspathCache::for_module(&working_dir);
        let cache_key = ClasspathCache::compute_key(&working_dir, &env_cfg);
        let cache_valid = cache.is_valid(&cache_key);
        let strategy = decide_build_strategy(&working_dir, &env_cfg, cache_valid);

        Self::emit_log(
            &app,
            &service.id,
            LogSource::Mvn,
            &format!(
                "[javaboot] 构建策略: {:?}（classpath cache: {}）",
                strategy,
                if cache_valid { "hit" } else { "miss" }
            ),
        );

        if strategy != BuildStrategy::Skip {
            try_cleanup!(
                self.run_maven_build(
                    &app, &service, &env_cfg, &working_dir, &program, &base_args,
                    &compile_pid, strategy,
                ).await
            );
        }
        check_cancel!();

        let classpath = if cache_valid {
            match cache.load() {
                Some(cp) => Self::assemble_classpath(&working_dir, &env_cfg, &cp),
                None => {
                    let cp = try_cleanup!(
                        self.resolve_classpath_via_mvn(
                            &app, &service, &env_cfg, &working_dir, &program, &base_args,
                            &compile_pid, &cache, &cache_key,
                        ).await
                    );
                    Self::assemble_classpath(&working_dir, &env_cfg, &cp)
                }
            }
        } else {
            let cp = try_cleanup!(
                self.resolve_classpath_via_mvn(
                    &app, &service, &env_cfg, &working_dir, &program, &base_args,
                    &compile_pid, &cache, &cache_key,
                ).await
            );
            Self::assemble_classpath(&working_dir, &env_cfg, &cp)
        };
        check_cancel!();

        let main_class = try_cleanup!(detect_main_class(&service, &working_dir));

        let java_home = resolve_java_home(&env_cfg);
        let java_bin = java_home
            .map(|jh| format!("{}\\bin\\java.exe", jh))
            .unwrap_or_else(|| "java".to_string());

        Self::emit_log(
            &app,
            &service.id,
            LogSource::Mvn,
            &format!(
                "[javaboot] 启动: {}{}{}",
                main_class,
                service.profiles.as_deref().map(|pf| format!(" [profiles={}]", pf.trim())).unwrap_or_default(),
                if service.dev_mode { " [dev_mode]" } else { "" },
            ),
        );

        let mut cmd = Command::new(&java_bin);
        cmd.arg("-Dfile.encoding=UTF-8");
        if service.dev_mode {
            cmd.arg("-XX:TieredStopAtLevel=1");
            cmd.arg("-XX:+AlwaysPreTouch");
            cmd.arg("-Dspring.jmx.enabled=false");
            cmd.arg("-Dspring.output.ansi.enabled=never");
            cmd.arg("-Dspring.devtools.restart.enabled=false");
        }
        if let Some(pf) = &service.profiles {
            if !pf.trim().is_empty() {
                cmd.arg(format!("-Dspring.profiles.active={}", pf.trim()));
            }
        }
        if let Some(mo) = &service.maven_opts {
            for a in mo.split_whitespace() {
                if a.starts_with("-D") || a.starts_with("-X") {
                    cmd.arg(a);
                }
            }
        }
        cmd.arg("-cp");
        cmd.arg(&classpath);
        cmd.arg(&main_class);
        if let Some(mo) = &service.maven_opts {
            for a in mo.split_whitespace() {
                if !a.starts_with("-D") && !a.starts_with("-X") {
                    cmd.arg(a);
                }
            }
        }
        cmd.current_dir(&working_dir);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.stdin(std::process::Stdio::null());
        inject_env(&mut cmd, &env_cfg);
        #[cfg(windows)]
        {
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
            cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
        }

        check_cancel!();

        let mut child = cmd.spawn().map_err(|e| {
            self.handles.lock().remove(&service.id);
            self.set_status(&app, &service.id, ServiceStatus::Stopped);
            AppError::Process(format!(
                "启动失败 ({}): {}\n请检查该服务配置的 JDK 路径和 Maven 是否可用。",
                program, e
            ))
        })?;
        let pid = child.id().unwrap_or(0);

        let job = JobObject::new().map_err(|e| {
            self.handles.lock().remove(&service.id);
            self.set_status(&app, &service.id, ServiceStatus::Stopped);
            AppError::Process(format!("Job Object 创建失败: {}", e))
        })?;
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::HANDLE;
            if let Some(h) = child.raw_handle() {
                let _ = job.assign(HANDLE(h));
            }
        }
        let job_arc = Arc::new(PMutex::new(job));
        {
            let mut handles = self.handles.lock();
            if let Some(h) = handles.get_mut(&service.id) {
                h.pid = pid;
                h.job = Some(job_arc.clone());
            }
        }
        self.set_pid(&app, &service.id, pid);

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        Self::spawn_log_reader(app.clone(), service.id.clone(), stdout, stderr);

        let app3 = app.clone();
        let sid3 = service.id.clone();
        let kill_token2 = kill_token.clone();
        tokio::spawn(async move {
            let status = child.wait().await;
            let killed = kill_token2.load(Ordering::Relaxed);
            if !killed {
                match status {
                    Ok(s) => {
                        if !s.success() {
                            Self::emit_log(
                                &app3, &sid3, LogSource::App,
                                &format!("[javaboot] 进程退出，退出码: {:?}", s.code()),
                            );
                            get_manager().set_status(&app3, &sid3, ServiceStatus::Error);
                        } else {
                            get_manager().set_status(&app3, &sid3, ServiceStatus::Stopped);
                        }
                    }
                    Err(e) => {
                        Self::emit_log(
                            &app3, &sid3, LogSource::App,
                            &format!("[javaboot] 进程等待错误: {}", e),
                        );
                        get_manager().set_status(&app3, &sid3, ServiceStatus::Error);
                    }
                }
            } else {
                get_manager().set_status(&app3, &sid3, ServiceStatus::Stopped);
            }
            get_manager().handles.lock().remove(&sid3);
            let _ = db::clear_run_pid(&sid3);
        });

        Ok(())
    }

    /// stdout / stderr 分别读取，不再合并 & 不再滑窗去重
    fn spawn_log_reader(
        app: AppHandle,
        service_id: String,
        stdout: Option<tokio::process::ChildStdout>,
        stderr: Option<tokio::process::ChildStderr>,
    ) {
        if let Some(out) = stdout {
            let app_c = app.clone();
            let sid_c = service_id.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(out).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    Self::emit_log(&app_c, &sid_c, LogSource::App, &line);
                    if check_started(&line) {
                        get_manager().mark_running(&app_c, &sid_c);
                    } else if check_failed(&line) {
                        get_manager().set_status(&app_c, &sid_c, ServiceStatus::Error);
                    }
                }
            });
        }
        if let Some(err) = stderr {
            tokio::spawn(async move {
                let mut reader = BufReader::new(err).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    Self::emit_log(&app, &service_id, LogSource::App, &line);
                    if check_failed(&line) {
                        get_manager().set_status(&app, &service_id, ServiceStatus::Error);
                    }
                }
            });
        }
    }

    // ================================================================
    // Maven 执行（按策略）
    // ================================================================

    async fn run_maven_build(
        &self,
        app: &AppHandle,
        service: &Service,
        env_cfg: &EnvConfig,
        working_dir: &std::path::Path,
        program: &str,
        base_args: &[String],
        compile_pid: &CompilePidSlot,
        strategy: BuildStrategy,
    ) -> AppResult<()> {
        let project_root = env_cfg.project_root.clone();
        let (cwd, module_rel) = match &project_root {
            Some(root)
                if strategy == BuildStrategy::CompileAll
                    || strategy == BuildStrategy::CompileCurrent =>
            {
                let root_path = std::path::Path::new(root);
                let rel = match working_dir.strip_prefix(root_path) {
                    Ok(r) => r.to_string_lossy().replace('\\', "/").trim_matches('/').to_string(),
                    Err(_) => {
                        let wd = working_dir.canonicalize().unwrap_or_default();
                        let pr = root_path.canonicalize().unwrap_or_default();
                        wd.strip_prefix(&pr)
                            .map(|r| r.to_string_lossy().replace('\\', "/").trim_matches('/').to_string())
                            .unwrap_or_default()
                    }
                };
                if rel.is_empty() {
                    (working_dir.to_path_buf(), String::new())
                } else {
                    (root_path.to_path_buf(), rel)
                }
            }
            _ => (working_dir.to_path_buf(), String::new()),
        };

        let mut args: Vec<String> = base_args.to_vec();
        args.extend(common_mvn_flags());
        args.push("compile".to_string());
        if !module_rel.is_empty() {
            args.push("-pl".into());
            args.push(module_rel.clone());
            if strategy == BuildStrategy::CompileAll {
                args.push("-am".into());
            }
        }

        Self::emit_log(
            app,
            &service.id,
            LogSource::Mvn,
            &format!(
                "[javaboot] {}: mvn {}",
                if module_rel.is_empty() {
                    "编译当前模块"
                } else if strategy == BuildStrategy::CompileAll {
                    "编译当前模块+依赖模块"
                } else {
                    "编译当前模块"
                },
                args.join(" ")
            ),
        );

        let program = program.to_string();
        let cwd_clone = cwd.clone();
        let env_cfg_clone = env_cfg.clone();
        let compile_pid_clone = compile_pid.clone();
        let app_clone = app.clone();
        let sid_clone = service.id.clone();

        let status = tokio::task::spawn_blocking(move || {
            run_mvn_capture(
                &program,
                &args,
                &cwd_clone,
                &env_cfg_clone,
                compile_pid_clone,
                app_clone,
                sid_clone,
            )
        })
        .await
        .map_err(|e| AppError::Process(format!("Maven 任务失败: {}", e)))?
        .map_err(|e| AppError::Process(format!("Maven 执行失败: {}", e)))?;

        if !status.success() {
            return Err(AppError::Process(format!(
                "Maven 编译失败（exit code: {:?}）",
                status.code()
            )));
        }
        Ok(())
    }

    /// 用 `mvn dependency:build-classpath` 拉全量依赖 classpath 并写入缓存
    async fn resolve_classpath_via_mvn(
        &self,
        app: &AppHandle,
        service: &Service,
        env_cfg: &EnvConfig,
        working_dir: &std::path::Path,
        program: &str,
        base_args: &[String],
        compile_pid: &CompilePidSlot,
        cache: &ClasspathCache,
        cache_key: &str,
    ) -> AppResult<String> {
        Self::emit_log(app, &service.id, LogSource::Mvn, "[javaboot] 解析依赖 classpath...");

        let _ = std::fs::create_dir_all(working_dir.join("target"));
        let cp_file = cache.cp_file.clone();

        let mut args: Vec<String> = base_args.to_vec();
        args.push("dependency:build-classpath".into());
        args.push(format!("-Dmdep.outputFile={}", cp_file.to_string_lossy()));
        args.push("--batch-mode".into());
        args.push("--no-transfer-progress".into());

        let program = program.to_string();
        let cwd = working_dir.to_path_buf();
        let env_cfg_clone = env_cfg.clone();
        let compile_pid_clone = compile_pid.clone();
        let app_clone = app.clone();
        let sid_clone = service.id.clone();

        let status = tokio::task::spawn_blocking(move || {
            run_mvn_capture(
                &program,
                &args,
                &cwd,
                &env_cfg_clone,
                compile_pid_clone,
                app_clone,
                sid_clone,
            )
        })
        .await
        .map_err(|e| AppError::Process(format!("classpath 任务失败: {}", e)))?
        .map_err(|e| AppError::Process(format!("classpath 执行失败: {}", e)))?;

        if !status.success() {
            return Err(AppError::Process(format!(
                "无法解析依赖 classpath（exit code: {:?}）",
                status.code()
            )));
        }

        let dep_cp = std::fs::read_to_string(&cp_file)
            .unwrap_or_default()
            .trim()
            .to_string();
        if dep_cp.is_empty() {
            return Err(AppError::Process("classpath 输出为空".into()));
        }
        if let Err(e) = cache.save(&dep_cp, cache_key) {
            log::warn!("写 classpath 缓存失败: {}", e);
        } else {
            Self::emit_log(
                app,
                &service.id,
                LogSource::Mvn,
                &format!("[javaboot] classpath 已缓存 ({} 字节)", dep_cp.len()),
            );
        }
        Ok(dep_cp)
    }

    /// 组装完整 classpath：target/classes + 兄弟模块 classes + jar 依赖
    fn assemble_classpath(
        working_dir: &std::path::Path,
        env_cfg: &EnvConfig,
        dep_cp: &str,
    ) -> String {
        let classes_dir = working_dir.join("target").join("classes");
        let mut parts: Vec<String> = vec![classes_dir.to_string_lossy().to_string()];
        if let Some(root) = &env_cfg.project_root {
            parts.extend(collect_sibling_classes(std::path::Path::new(root), working_dir));
        }
        if !dep_cp.is_empty() {
            parts.push(dep_cp.to_string());
        }
        parts.join(";")
    }

    // ================================================================
    // stop / restart / compile_and_start / stop_all
    // ================================================================

    pub async fn stop(&self, app: AppHandle, service_id: &str) -> AppResult<()> {
        Self::emit_log(&app, service_id, LogSource::Mvn, "[javaboot] 正在停止服务...");
        let handle = {
            let mut handles = self.handles.lock();
            handles.remove(service_id)
        };
        if let Some(h) = handle {
            h.kill_token.store(true, Ordering::Relaxed);
            self.set_status(&app, service_id, ServiceStatus::Stopping);
            if let Some(cpid) = h.compile_pid.lock().take() {
                kill_process_tree_by_pid(cpid);
            }
            if let Some(job) = h.job {
                job.lock().kill();
            } else if h.pid > 0 {
                kill_process_tree_by_pid(h.pid);
            }
            let _ = db::clear_run_pid(service_id);
            self.set_status(&app, service_id, ServiceStatus::Stopped);
            Self::emit_log(&app, service_id, LogSource::Mvn, "[javaboot] 服务已停止");
        } else {
            let pid = self.get_runtime(service_id).pid;
            if let Some(pid) = pid {
                self.set_status(&app, service_id, ServiceStatus::Stopping);
                kill_process_tree_by_pid(pid);
                let _ = db::clear_run_pid(service_id);
                self.set_status(&app, service_id, ServiceStatus::Stopped);
                Self::emit_log(&app, service_id, LogSource::Mvn, "[javaboot] 服务已停止");
            } else {
                self.set_status(&app, service_id, ServiceStatus::Stopped);
                Self::emit_log(&app, service_id, LogSource::Mvn, "[javaboot] 服务未在运行");
            }
        }
        Ok(())
    }

    pub async fn restart(&self, app: AppHandle, service: Service) -> AppResult<()> {
        let old_ports = self.get_runtime(&service.id).ports;
        self.stop(app.clone(), &service.id).await?;
        if !old_ports.is_empty() {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                let listening = crate::port::all_listening_ports().unwrap_or_default();
                let listening_ports: std::collections::HashSet<u16> =
                    listening.into_iter().map(|(p, _)| p).collect();
                if !old_ports.iter().any(|p| listening_ports.contains(p)) {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    log::warn!("重启：等待端口释放超时，继续启动");
                    break;
                }
            }
        } else {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
        self.start(app, service).await
    }

    /// 编译并启动：由 watcher 触发的自动重启流程
    ///
    /// 直接走 start()：内部会通过 mtime 判定选择合适的编译动作，
    /// 与手动 mvn compile 语义等价，且共享 classpath 缓存 / dev_mode / 三档策略。
    pub async fn compile_and_start(&self, app: AppHandle, service: Service) -> AppResult<()> {
        self.stop(app.clone(), &service.id).await?;
        self.set_status(&app, &service.id, ServiceStatus::Recompiling);
        self.start(app, service).await
    }

    /// 停止所有运行中的服务（并发）
    pub async fn stop_all(&self, app: AppHandle) -> AppResult<()> {
        let ids: Vec<String> = self.handles.lock().keys().cloned().collect();
        let count = ids.len();
        if count == 0 {
            return Ok(());
        }
        Self::emit_log_static(
            &app,
            &ids[0],
            "[javaboot]",
            &format!("[javaboot] 正在停止全部 {} 个服务...", count),
        );
        let futures: Vec<_> = ids
            .into_iter()
            .map(|id| {
                let app = app.clone();
                async move { get_manager().stop(app, &id).await }
            })
            .collect();
        for res in futures::future::join_all(futures).await {
            if let Err(e) = res {
                log::warn!("stop_all: {}", e);
            }
        }
        Ok(())
    }
}

// 防止 build::pom_newer_than 被判死代码
#[allow(dead_code)]
fn _keep_build_helpers() {
    let _ = build::pom_newer_than;
}

// ================================================================
// 全局单例
// ================================================================

static MANAGER: Lazy<ProcessManager> = Lazy::new(ProcessManager::new);

pub fn get_manager() -> &'static ProcessManager {
    &MANAGER
}

static SYS: Lazy<PMutex<sysinfo::System>> = Lazy::new(|| PMutex::new(sysinfo::System::new()));
