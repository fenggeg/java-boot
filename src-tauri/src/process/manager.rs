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
    run_mvn_capture, strip_verbatim_prefix, BuildStrategy, ClasspathCache, CompilePidSlot,
};
use super::env::{
    inject_env, preflight_check, resolve_env_config, resolve_java_home, resolve_maven_cmd,
    EnvConfig,
};
use super::job::JobObject;
use super::log_pipe::{check_failed, check_started, emit_log_raw, extract_service_ports, LogSource};

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
    /// handle 创建时间；用于识别“死 placeholder”（pid=0 且长时间未推进）
    created_at: std::time::Instant,
}

impl ProcessHandle {
    fn placeholder() -> Self {
        Self {
            pid: 0,
            job: None,
            kill_token: Arc::new(AtomicBool::new(false)),
            compile_pid: Arc::new(PMutex::new(None)),
            created_at: std::time::Instant::now(),
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
    ///
    /// `ports` 字段保留 PID 下所有 LISTENING 端口（含 JMX/RMI/H2 等噪声端口），
    /// 用于冲突检测；前端展示用 `service_ports`（从 Spring Boot 启动日志解析）。
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
            // 去重：IPv4/IPv6 双栈绑定会让同一端口对同一 PID 出现两次
            let vec = pid_ports.entry(*owner_pid).or_default();
            if !vec.contains(port) {
                vec.push(*port);
            }
        }
        let mut changed = false;
        {
            let mut rt = self.runtimes.lock();
            for (service_id, pid) in &service_pids {
                let all_ports = pid_ports.get(pid).cloned().unwrap_or_default();
                let entry = rt.entry(service_id.clone()).or_default();
                entry.service_id = service_id.clone();
                if entry.ports != all_ports {
                    entry.ports = all_ports;
                    changed = true;
                }
                // service_ports 由日志解析设置，这里不覆盖；前端展示时若 service_ports
                // 为空则回退到 ports（兜底，保证日志未匹配上时也有显示）
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
            entry.service_ports.clear();
            entry.port_conflict = false;
            entry.conflict_with.clear();
            // 清掉陈旧的 CPU/内存，避免 UI 在停止/异常状态下仍显示上一次的数值
            entry.cpu_usage = None;
            entry.memory_mb = None;
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

    /// 设置从 Spring Boot 启动日志解析出的 HTTP 服务端口
    ///
    /// 前端会优先展示 `service_ports`；为空时回退到 `ports`（PID 所有 LISTENING 端口）。
    fn set_service_ports(&self, app: &AppHandle, service_id: &str, ports: Vec<u16>) {
        let mut rt = self.runtimes.lock();
        let entry = rt.entry(service_id.to_string()).or_default();
        entry.service_id = service_id.to_string();
        // 多次匹配（多 web server 启动）累加去重
        for p in &ports {
            if !entry.service_ports.contains(p) {
                entry.service_ports.push(*p);
            }
        }
        let snapshot = entry.clone();
        drop(rt);
        let _ = app.emit("service://status", snapshot);
    }

    // ================================================================
    // start：核心启动流程（三档策略 + classpath 缓存 + dev_mode）
    // ================================================================

    pub async fn start(&self, app: AppHandle, service: Service) -> AppResult<()> {
        let (kill_token, compile_pid) = {
            let mut handles = self.handles.lock();
            if let Some(existing) = handles.get(&service.id) {
                // 已有条目：细分三种情况
                //   (a) pid > 0 且 sysinfo 查得到       → 真运行中，拒绝
                //   (b) pid > 0 但进程已死（僵尸）     → 静默清理后继续
                //   (c) pid == 0（placeholder）：
                //         - kill_token 已 signal → 已被 stop，残留→清理
                //         - 创建超过 5 分钟未推进  → 死 placeholder →清理
                //         - 否则看作并发启动中       →拒绝
                let alive = if existing.pid > 0 {
                    let pid = sysinfo::Pid::from_u32(existing.pid);
                    let mut sys = SYS.lock();
                    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), false);
                    sys.process(pid).is_some()
                } else {
                    let signaled = existing.kill_token.load(Ordering::Relaxed);
                    let stale = existing.created_at.elapsed() > std::time::Duration::from_secs(300);
                    !(signaled || stale)
                };
                if alive {
                    return Err(AppError::ServiceRunning(service.id));
                }
                log::info!(
                    "堆叠残留 handle(PID {}, elapsed {:?})，已自动清理：{}",
                    existing.pid,
                    existing.created_at.elapsed(),
                    service.id
                );
                // 主动 signal，避免可能还在后台卡着的旧 async task 拉长系统状态
                existing.kill_token.store(true, Ordering::Relaxed);
                handles.remove(&service.id);
                let _ = db::clear_run_pid(&service.id);
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

        let working_dir = strip_verbatim_prefix(&PathBuf::from(&service.working_dir));
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
                Some(cp) => {
                    let jars = cp.split(';').filter(|s| !s.is_empty()).count();
                    Self::emit_log(
                        &app,
                        &service.id,
                        LogSource::Mvn,
                        &format!("[javaboot] classpath 缓存已加载 ({} 个依赖)", jars),
                    );
                    Self::assemble_classpath(&working_dir, &env_cfg, &cp)
                }
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

        Self::emit_log(&app, &service.id, LogSource::Mvn, "[javaboot] 探测主类...");
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

        // 收集所有 java 参数，便于按需走 @argfile（Windows 命令行上限 32767 字符）
        let mut args: Vec<String> = vec!["-Dfile.encoding=UTF-8".to_string()];
        if service.dev_mode {
            args.extend([
                "-XX:TieredStopAtLevel=1".into(),
                "-XX:+AlwaysPreTouch".into(),
                "-Dspring.jmx.enabled=false".into(),
                "-Dspring.output.ansi.enabled=never".into(),
                "-Dspring.devtools.restart.enabled=false".into(),
            ]);
        }
        if let Some(pf) = &service.profiles {
            if !pf.trim().is_empty() {
                args.push(format!("-Dspring.profiles.active={}", pf.trim()));
            }
        }
        if let Some(mo) = &service.maven_opts {
            for a in mo.split_whitespace() {
                if a.starts_with("-D") || a.starts_with("-X") {
                    args.push(a.to_string());
                }
            }
        }
        // 配置覆盖属性：JSON → -Dkey=value，放在 maven_opts 的 -D 之后，
        // 确保用户在 UI 里配置的覆盖值优先级最高（Spring Boot 系统属性优先于 application.yml）
        if let Some(overrides) = parse_override_properties(&service.override_properties) {
            if !overrides.is_empty() {
                for (k, v) in &overrides {
                    args.push(format!("-D{}={}", k, v));
                }
                Self::emit_log(
                    &app,
                    &service.id,
                    LogSource::Mvn,
                    &format!("[javaboot] 注入 {} 个覆盖属性", overrides.len()),
                );
            }
        }
        args.push("-cp".into());
        args.push(classpath.clone());
        args.push(main_class.clone());
        if let Some(mo) = &service.maven_opts {
            for a in mo.split_whitespace() {
                if !a.starts_with("-D") && !a.starts_with("-X") {
                    args.push(a.to_string());
                }
            }
        }

        // 估算命令行长度：java_bin + 各 arg + 分隔符
        let cmd_len = java_bin.len() + 1 + args.iter().map(|a| a.len() + 1).sum::<usize>();
        // Windows CreateProcessW 命令行上限 32767 字符；留余量给 quoting/program
        let use_argfile = cmd_len > 30000;

        let mut cmd = Command::new(&java_bin);
        if use_argfile {
            // classpath 太长，写入 @argfile 启动（Java 原生支持）
            let argfile_path = working_dir.join("target").join(".javaboot-args.txt");
            let mut content = String::new();
            for arg in &args {
                // argfile 里含空格/tab 的参数需要双引号包裹；路径里的反斜杠在引号内是字面量
                if arg.contains(' ') || arg.contains('\t') {
                    let escaped = arg.replace('"', "\\\"");
                    content.push('"');
                    content.push_str(&escaped);
                    content.push('"');
                } else {
                    content.push_str(arg);
                }
                content.push('\n');
            }
            if let Some(parent) = argfile_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = std::fs::write(&argfile_path, &content) {
                self.handles.lock().remove(&service.id);
                self.set_status(&app, &service.id, ServiceStatus::Stopped);
                return Err(AppError::Process(format!(
                    "写入 argfile 失败 ({}): {}",
                    argfile_path.display(), e
                )));
            }
            cmd.arg(format!("@{}", argfile_path.to_string_lossy()));
            Self::emit_log(
                &app,
                &service.id,
                LogSource::Mvn,
                &format!(
                    "[javaboot] 命令行较长 ({} 字符)，使用 @argfile 启动: {}",
                    cmd_len,
                    argfile_path.file_name().unwrap_or_default().to_string_lossy()
                ),
            );
        } else {
            for arg in &args {
                cmd.arg(arg);
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

        Self::emit_log(&app, &service.id, LogSource::Mvn, "[javaboot] 启动 java 子进程...");
        let mut child = cmd.spawn().map_err(|e| {
            let msg = format!("启动失败 ({}): {}", program, e);
            // 同步推到日志面板，避免只在前端 toast 看到，日志面板却一片空白
            Self::emit_log(&app, &service.id, LogSource::Mvn, &format!("[javaboot] {}", msg));
            self.handles.lock().remove(&service.id);
            self.set_status(&app, &service.id, ServiceStatus::Stopped);
            AppError::Process(format!(
                "{}\n请检查该服务配置的 JDK 路径和 Maven 是否可用。\n（若 classpath 过长，已自动切换 @argfile；若仍失败请检查 target/.javaboot-args.txt）",
                msg
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
                        // 不论成功失败都打退出码，避免"进程默默退出、日志一片空白"的情况
                        Self::emit_log(
                            &app3, &sid3, LogSource::App,
                            &format!("[javaboot] 进程退出，退出码: {:?}", s.code()),
                        );
                        if !s.success() {
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
                        // 从 Spring Boot 启动日志中解析 HTTP 服务端口，覆盖噪声端口
                        let ports = extract_service_ports(&line);
                        if !ports.is_empty() {
                            get_manager().set_service_ports(&app_c, &sid_c, ports);
                        }
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
// 配置覆盖属性解析
// ================================================================

/// 解析 service.override_properties（JSON）为有序 (key, value) 列表。
///
/// JSON 格式：`[{"key":"spring.cloud.nacos.discovery.ip","value":"192.168.1.100"}]`
/// 跳过 key 为空或解析失败的条目；解析失败时返回 None（不影响启动）。
fn parse_override_properties(json: &Option<String>) -> Option<Vec<(String, String)>> {
    let raw = json.as_ref()?.trim();
    if raw.is_empty() {
        return None;
    }
    #[derive(serde::Deserialize)]
    struct Kv {
        key: String,
        value: String,
    }
    match serde_json::from_str::<Vec<Kv>>(raw) {
        Ok(list) => {
            let out: Vec<(String, String)> = list
                .into_iter()
                .filter(|kv| !kv.key.trim().is_empty())
                .map(|kv| (kv.key.trim().to_string(), kv.value))
                .collect();
            Some(out)
        }
        Err(e) => {
            log::warn!("override_properties JSON 解析失败: {}", e);
            None
        }
    }
}

// ================================================================
// 全局单例
// ================================================================

static MANAGER: Lazy<ProcessManager> = Lazy::new(ProcessManager::new);

pub fn get_manager() -> &'static ProcessManager {
    &MANAGER
}

static SYS: Lazy<PMutex<sysinfo::System>> = Lazy::new(|| PMutex::new(sysinfo::System::new()));
