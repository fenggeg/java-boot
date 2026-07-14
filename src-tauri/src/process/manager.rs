use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use parking_lot::Mutex as PMutex;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::db;
use crate::db::models::{Service, ServiceRuntime, ServiceStatus};
use crate::error::AppResult;
use crate::port;

use super::job::JobObject;

/// 启动成功检测正则（兼容 SpringBoot 2.x/3.x）
const STARTED_PATTERNS: &[&str] = &[
    "Started Application in",
    "Started .* in .* seconds",
    "Tomcat started on port",
    "Tomcat initialized",
    "Jetty started on port",
    "Netty started on port",
    "APPLICATION FAILED TO START",
];

#[derive(Clone, Copy, PartialEq)]
enum LogSource {
    App,
    Mvn,
}

impl LogSource {
    fn tag(&self) -> &'static str {
        match self {
            LogSource::App => "[app]",
            LogSource::Mvn => "[mvn]",
        }
    }
}

struct ProcessHandle {
    #[allow(dead_code)]
    pid: u32,
    job: Arc<PMutex<JobObject>>,
    kill_token: Arc<PMutex<bool>>,
}

/// 全局进程管理器
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

        let mut sys = sysinfo::System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

        for (service_id, pid, started_at) in pids {
            // 检查 PID 是否还活着
            if let Some(_proc) = sys.process(sysinfo::Pid::from_u32(pid)) {
                // 进程还在，恢复运行状态
                log::info!("恢复服务 {} (PID {})", service_id, pid);
                let mut rt = self.runtimes.lock();
                let entry = rt.entry(service_id.clone()).or_default();
                entry.service_id = service_id.clone();
                entry.status = ServiceStatus::Running;
                entry.pid = Some(pid);
                entry.started_at = Some(started_at);

                // 查端口
                if let Ok(ports) = crate::port::ports_for_pid(pid) {
                    if !ports.is_empty() {
                        entry.ports = ports;
                    }
                }
            } else {
                // 进程已死，清理
                let _ = db::clear_run_pid(&service_id);
                log::info!("服务 {} 的进程已不存在，清理", service_id);
            }
        }

        // 推送状态到前端
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

        let mut sys = sysinfo::System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

        let mut rt = self.runtimes.lock();
        for (service_id, pid) in pids {
            if let Some(proc) = sys.process(sysinfo::Pid::from_u32(pid)) {
                let entry = rt.entry(service_id.clone()).or_default();
                entry.service_id = service_id.clone();
                entry.cpu_usage = Some(proc.cpu_usage());
                entry.memory_mb = Some(proc.memory() as f64 / 1024.0 / 1024.0);
            }
        }
        drop(rt);

        // 推送到前端
        for rt in self.all_runtimes() {
            let _ = app.emit("service://status", rt);
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

    fn set_ports(&self, app: &AppHandle, service_id: &str, ports: Vec<u16>) {
        let mut rt = self.runtimes.lock();
        let entry = rt.entry(service_id.to_string()).or_default();
        entry.service_id = service_id.to_string();
        entry.ports = ports;
        let snapshot = entry.clone();
        drop(rt);
        let _ = app.emit("service://status", snapshot);
    }

    /// 标记端口冲突
    pub fn refresh_port_conflicts(&self, app: &AppHandle) {
        let mut rt = self.runtimes.lock();
        // pid -> service_id
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

    /// 启动服务
    pub async fn start(&self, app: AppHandle, service: Service) -> AppResult<()> {
        {
            let handles = self.handles.lock();
            if handles.contains_key(&service.id) {
                return Err(crate::error::AppError::ServiceRunning(service.id));
            }
        }
        self.set_status(&app, &service.id, ServiceStatus::Starting);

        let working_dir = PathBuf::from(&service.working_dir);
        // 从项目获取 JDK / Maven 配置
        let env_cfg = resolve_env_config(&service)?;
        let (program, base_args) = resolve_maven_cmd(&working_dir, &env_cfg);

        // 预检：确认 java / mvn 可用
        preflight_check(&env_cfg, &working_dir, &program)?;

        // 多模块项目：先 install 当前模块及依赖模块
        self.prepare_dependencies(&app, &service, &env_cfg, &working_dir, &program, &base_args).await?;

        // 获取完整 classpath（绕过 spring-boot-maven-plugin 的 <includes> 限制）
        // = target/classes + 兄弟模块 target/classes + 所有 jar 依赖
        let classpath = self.build_classpath(&app, &service, &env_cfg, &working_dir, &program, &base_args).await?;
        let java_home = resolve_java_home(&env_cfg);
        let java_bin = java_home
            .map(|jh| format!("{}\\bin\\java.exe", jh))
            .unwrap_or_else(|| "java".to_string());

        // 探测 mainClass
        let main_class = detect_main_class(&service, &working_dir);

        Self::emit_log(
            &app,
            &service.id,
            LogSource::Mvn,
            &format!("[javaboot] 启动: {} {}", main_class, if let Some(pf) = &service.profiles { format!("[profiles={}]", pf) } else { String::new() }),
        );

        let mut cmd = Command::new(&java_bin);
        // JVM 参数
        cmd.arg("-Dfile.encoding=UTF-8");
        // Spring profiles
        if let Some(pf) = &service.profiles {
            if !pf.trim().is_empty() {
                cmd.arg(format!("-Dspring.profiles.active={}", pf.trim()));
            }
        }
        // 额外 Maven 参数里的 -D/-X 开头的当 JVM 参数
        if let Some(mo) = &service.maven_opts {
            if !mo.trim().is_empty() {
                for a in mo.split_whitespace() {
                    if a.starts_with("-D") || a.starts_with("-X") {
                        cmd.arg(a);
                    }
                }
            }
        }
        cmd.arg("-cp");
        cmd.arg(&classpath);
        cmd.arg(&main_class);
        // 非 JVM 的额外参数（不以 -D/-X 开头的）
        if let Some(mo) = &service.maven_opts {
            if !mo.trim().is_empty() {
                for a in mo.split_whitespace() {
                    if !a.starts_with("-D") && !a.starts_with("-X") {
                        cmd.arg(a);
                    }
                }
            }
        }
        cmd.current_dir(&working_dir);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.stdin(std::process::Stdio::null());
        // 显式注入环境变量（确保 JAVA_HOME / PATH 正确）
        inject_env(&mut cmd, &env_cfg);
        // Windows: 创建新进程组，避免 Ctrl+C 信号传播到其他服务
        #[cfg(windows)]
        {
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
            cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| crate::error::AppError::Process(format!(
                "启动失败 ({}): {}\n请检查该服务配置的 JDK 路径和 Maven 是否可用。",
                program, e
            )))?;

        let pid = child.id().unwrap_or(0);

        // 创建 Job Object 并把进程加入
        let job = JobObject::new().map_err(|e| {
            crate::error::AppError::Process(format!("Job Object 创建失败: {}", e))
        })?;
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::HANDLE;
            // tokio::process::Child 提供 raw_handle() 返回 Option<*mut c_void>
            let raw = child.raw_handle();
            if let Some(h) = raw {
                let _ = job.assign(HANDLE(h));
            }
        }
        let job_arc = Arc::new(PMutex::new(job));
        let kill_token = Arc::new(PMutex::new(false));

        self.handles.lock().insert(
            service.id.clone(),
            ProcessHandle {
                pid,
                job: job_arc,
                kill_token: kill_token.clone(),
            },
        );

        self.set_pid(&app, &service.id, pid);

        // 读取 stdout/stderr：合并为一个流避免重复（Maven 会同时写两边）
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let app_clone = app.clone();
        let sid_clone = service.id.clone();
        // 用 channel 合并两个流的行，保证顺序且不重复
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        if let Some(out) = stdout {
            let tx2 = tx.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(out).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    let _ = tx2.send(line);
                }
            });
        }
        if let Some(err) = stderr {
            let tx2 = tx.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(err).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    let _ = tx2.send(line);
                }
            });
        }
        drop(tx); // 关闭发送端，rx 才能结束
        tokio::spawn(async move {
            // 滑动窗口去重：Maven/SpringBoot 同时往 stdout/stderr 写相同行，
            // 两个流并行读取导致行交错，相邻去重无效。用最近 N 行的窗口去重。
            let mut recent: std::collections::VecDeque<String> = std::collections::VecDeque::with_capacity(32);
            while let Some(line) = rx.recv().await {
                if recent.iter().any(|l| l == &line) {
                    continue; // 在窗口内已存在，跳过重复
                }
                recent.push_back(line.clone());
                if recent.len() > 32 {
                    recent.pop_front();
                }
                Self::emit_log(&app_clone, &sid_clone, LogSource::App, &line);
                if check_started(&line) {
                    let mgr = get_manager();
                    mgr.mark_running(&app_clone, &sid_clone);
                }
            }
        });

        // 端口轮询 + 进程退出监控
        let app3 = app.clone();
        let sid3 = service.id.clone();
        let cfg = db::load_config().unwrap_or_default();
        let interval = cfg.port_refresh_interval_secs;
        let kill_token2 = kill_token.clone();
        tokio::spawn(async move {
            // 等待进程退出
            let status = child.wait().await;
            // 如果是主动 kill（token 已设为 true），不算异常
            let killed = *kill_token2.lock();
            if !killed {
                let app_clone = app3.clone();
                let sid_clone = sid3.clone();
                match status {
                    Ok(s) => {
                        if !s.success() {
                            Self::emit_log(
                                &app_clone,
                                &sid_clone,
                                LogSource::App,
                                &format!("[javaboot] 进程退出，退出码: {:?}", s.code()),
                            );
                            get_manager().set_status(&app_clone, &sid_clone, ServiceStatus::Error);
                        } else {
                            get_manager().set_status(&app_clone, &sid_clone, ServiceStatus::Stopped);
                        }
                    }
                    Err(e) => {
                        Self::emit_log(
                            &app_clone,
                            &sid_clone,
                            LogSource::App,
                            &format!("[javaboot] 进程等待错误: {}", e),
                        );
                        get_manager().set_status(&app_clone, &sid_clone, ServiceStatus::Error);
                    }
                }
            } else {
                get_manager().set_status(&app3, &sid3, ServiceStatus::Stopped);
            }
            // 清理 handle
            {
                let mut handles = get_manager().handles.lock();
                handles.remove(&sid3);
            }
            let _ = db::clear_run_pid(&sid3);
        });

        // 端口轮询任务
        let app4 = app.clone();
        let sid4 = service.id.clone();
        let mgr = get_manager_ref();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
                let rt = mgr.get_runtime(&sid4);
                if rt.status == ServiceStatus::Stopped
                    || rt.status == ServiceStatus::Error
                    || rt.status == ServiceStatus::Stopping
                {
                    break;
                }
                if let Some(pid) = rt.pid {
                    if let Ok(ports) = port::ports_for_pid(pid) {
                        if !ports.is_empty() {
                            mgr.set_ports(&app4, &sid4, ports);
                            mgr.refresh_port_conflicts(&app4);
                        }
                    }
                }
            }
        });

        Ok(())
    }

    /// 启动前准备依赖：多模块项目先 install 当前模块及其依赖模块
    async fn prepare_dependencies(
        &self,
        app: &AppHandle,
        service: &Service,
        env_cfg: &EnvConfig,
        working_dir: &std::path::Path,
        program: &str,
        base_args: &[String],
    ) -> AppResult<()> {
        // 仅当有 project_root 且 working_dir 是子模块（working_dir != project_root）时才 install
        let project_root = match &env_cfg.project_root {
            Some(root) => root.clone(),
            None => return Ok(()), // 无项目归属，跳过
        };
        // 规范化路径比较（统一分隔符和大小写）
        let norm = |p: &str| -> String {
            std::path::Path::new(p)
                .canonicalize()
                .unwrap_or_else(|_| std::path::PathBuf::from(p))
                .to_string_lossy()
                .to_lowercase()
                .replace('\\', "/")
        };
        if norm(&working_dir.to_string_lossy()) == norm(&project_root) {
            // 单模块项目，不需要预 install
            return Ok(());
        }

        // 计算模块相对路径（如 scs-auth）
        let project_root_path = std::path::Path::new(&project_root);
        let module_rel = match working_dir.strip_prefix(project_root_path) {
            Ok(rel) => rel.to_string_lossy().replace('\\', "/").trim_matches('/').to_string(),
            Err(_) => {
                // strip_prefix 失败（路径格式不一致），用 canonicalize 再试
                let wd_canon = working_dir.canonicalize().unwrap_or_default();
                let pr_canon = project_root_path.canonicalize().unwrap_or_default();
                match wd_canon.strip_prefix(&pr_canon) {
                    Ok(rel) => rel.to_string_lossy().replace('\\', "/").trim_matches('/').to_string(),
                    Err(_) => return Ok(()), // 仍无法计算，跳过
                }
            }
        };
        if module_rel.is_empty() {
            return Ok(());
        }

        Self::emit_log(
            app,
            &service.id,
            LogSource::Mvn,
            &format!("[javaboot] 检测到多模块项目，预编译 {} 及依赖模块...", module_rel),
        );

        // 在项目根目录跑 mvn install -DskipTests -pl <module> -am
        let mut cmd = std::process::Command::new(program);
        cmd.args(base_args);
        cmd.arg("install");
        cmd.arg("-DskipTests");
        cmd.arg("-pl");
        cmd.arg(&module_rel);
        cmd.arg("-am"); // also make：构建依赖的兄弟模块
        cmd.arg("-q"); // quiet：减少日志噪音（只保留 WARNING/ERROR）
        cmd.current_dir(&project_root);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.stdin(std::process::Stdio::null());
        inject_env_std(&mut cmd, env_cfg);

        let sid = service.id.clone();
        let app_clone = app.clone();

        let result = tokio::task::spawn_blocking(move || {
            let mut child = cmd.spawn()?;
            // 实时读取输出
            if let Some(out) = child.stdout.take() {
                let reader = std::io::BufReader::new(out);
                use std::io::BufRead;
                for line in reader.lines().flatten() {
                    ProcessManager::emit_log_static(&app_clone, &sid, "[mvn]", &line);
                }
            }
            if let Some(err) = child.stderr.take() {
                let reader = std::io::BufReader::new(err);
                use std::io::BufRead;
                for line in reader.lines().flatten() {
                    ProcessManager::emit_log_static(&app_clone, &sid, "[mvn]", &line);
                }
            }
            let status = child.wait()?;
            Ok::<_, std::io::Error>(status)
        })
        .await
        .map_err(|e| crate::error::AppError::Process(format!("依赖预编译任务失败: {}", e)))?;

        match result {
            Ok(status) if status.success() => {
                Self::emit_log(
                    app,
                    &service.id,
                    LogSource::Mvn,
                    "[javaboot] 依赖预编译完成，开始启动服务...",
                );
                Ok(())
            }
            Ok(status) => {
                Self::emit_log(
                    app,
                    &service.id,
                    LogSource::Mvn,
                    &format!("[javaboot] 依赖预编译失败（exit code: {:?}），仍尝试直接启动", status.code()),
                );
                // 不阻断，让 spring-boot:run 尝试（有时 install 失败但 run 仍可成功）
                Ok(())
            }
            Err(e) => {
                Self::emit_log(
                    app,
                    &service.id,
                    LogSource::Mvn,
                    &format!("[javaboot] 依赖预编译执行失败: {}，仍尝试直接启动", e),
                );
                Ok(())
            }
        }
    }

    /// 用 mvn dependency:build-classpath 获取完整依赖 classpath
    async fn build_classpath(
        &self,
        app: &AppHandle,
        service: &Service,
        env_cfg: &EnvConfig,
        working_dir: &std::path::Path,
        program: &str,
        base_args: &[String],
    ) -> AppResult<String> {
        Self::emit_log(
            app,
            &service.id,
            LogSource::Mvn,
            "[javaboot] 解析依赖 classpath...",
        );

        let sid = service.id.clone();
        let app_clone = app.clone();

        // 用临时文件接收 classpath（可靠，不受 stdout 格式影响）
        let cp_file = working_dir.join("target").join(".javaboot-cp.txt");
        // 确保 target 目录存在
        let _ = std::fs::create_dir_all(working_dir.join("target"));

        let mut cmd = std::process::Command::new(program);
        cmd.args(base_args);
        cmd.arg("dependency:build-classpath");
        cmd.arg(format!("-Dmdep.outputFile={}", cp_file.to_string_lossy()));
        cmd.arg("--batch-mode"); // 非交互，不下载进度条
        // 不加 -q，保留输出用于诊断
        cmd.current_dir(working_dir);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.stdin(std::process::Stdio::null());
        inject_env_std(&mut cmd, env_cfg);

        let cp_file_clone = cp_file.clone();
        let result = tokio::task::spawn_blocking(move || {
            let mut child = cmd.spawn()?;
            let out = child.stdout.take().unwrap();
            let err = child.stderr.take().unwrap();
            let out_reader = std::io::BufReader::new(out);
            let err_reader = std::io::BufReader::new(err);
            use std::io::BufRead;
            let stdout_lines: Vec<String> = out_reader.lines().flatten().collect();
            let stderr_lines: Vec<String> = err_reader.lines().flatten().collect();
            let status = child.wait()?;
            let cp = std::fs::read_to_string(&cp_file_clone).unwrap_or_default();
            Ok::<_, std::io::Error>((stdout_lines, stderr_lines, status, cp))
        })
        .await
        .map_err(|e| crate::error::AppError::Process(format!("classpath 解析任务失败: {}", e)))?
        .map_err(|e| crate::error::AppError::Process(format!("classpath 解析失败: {}", e)))?;

        let (stdout_lines, stderr_lines, status, dep_cp) = result;
        let dep_cp = dep_cp.trim().to_string();

        // 只在 classpath 解析失败时才打印 stderr（避免 git-commit-id 等插件的噪音日志）
        if dep_cp.is_empty() {
            for line in &stderr_lines {
                Self::emit_log(&app_clone, &sid, LogSource::Mvn, line);
            }
            let stdout_tail: Vec<&String> = stdout_lines.iter().rev().take(10).collect();
            for line in stdout_tail.into_iter().rev() {
                Self::emit_log(&app_clone, &sid, LogSource::Mvn, line);
            }
            if !status.success() {
                return Err(crate::error::AppError::Process(
                    format!("无法解析依赖 classpath（exit code: {:?}）", status.code()),
                ));
            }
        }

        // 扫描项目下所有兄弟模块的 target/classes（多模块项目）
        let mut extra_cp: Vec<String> = vec![];
        if let Some(root) = &env_cfg.project_root {
            if let Ok(entries) = std::fs::read_dir(root) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_dir() {
                        let tc = p.join("target").join("classes");
                        if tc.exists() && tc != working_dir.join("target").join("classes") {
                            extra_cp.push(tc.to_string_lossy().to_string());
                        }
                        // 嵌套一层（如 scs-common/scs-common-core）
                        if let Ok(sub_entries) = std::fs::read_dir(&p) {
                            for sub in sub_entries.flatten() {
                                let sp = sub.path();
                                let tc = sp.join("target").join("classes");
                                if tc.exists() && tc != working_dir.join("target").join("classes") {
                                    extra_cp.push(tc.to_string_lossy().to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        Self::emit_log(
            &app_clone,
            &sid,
            LogSource::Mvn,
            &format!("[javaboot] classpath: jar 依赖 {} 字符, 兄弟模块 {} 个", dep_cp.len(), extra_cp.len()),
        );

        // 拼接：target/classes + 兄弟模块 classes + jar 依赖
        let classes_dir = working_dir.join("target").join("classes");
        let mut parts: Vec<String> = vec![classes_dir.to_string_lossy().to_string()];
        parts.extend(extra_cp);
        if !dep_cp.is_empty() {
            parts.push(dep_cp);
        }
        let full_cp = parts.join(";");

        Ok(full_cp)
    }

    fn mark_running(&self, app: &AppHandle, service_id: &str) {
        self.set_status(app, service_id, ServiceStatus::Running);
    }

    /// 停止服务（杀整个进程树）
    pub async fn stop(&self, app: AppHandle, service_id: &str) -> AppResult<()> {
        Self::emit_log(&app, service_id, LogSource::Mvn, "[javaboot] 正在停止服务...");
        let handle = {
            let mut handles = self.handles.lock();
            handles.remove(service_id)
        };
        if let Some(h) = handle {
            *h.kill_token.lock() = true;
            self.set_status(&app, service_id, ServiceStatus::Stopping);
            // 关闭 Job Object 会杀掉整个进程树
            h.job.lock().kill();
            let _ = db::clear_run_pid(service_id);
            self.set_status(&app, service_id, ServiceStatus::Stopped);
            Self::emit_log(&app, service_id, LogSource::Mvn, "[javaboot] 服务已停止");
        } else {
            // 已无 handle，确保状态置为 Stopped
            self.set_status(&app, service_id, ServiceStatus::Stopped);
            Self::emit_log(&app, service_id, LogSource::Mvn, "[javaboot] 服务未在运行");
        }
        Ok(())
    }

    /// 重启
    pub async fn restart(&self, app: AppHandle, service: Service) -> AppResult<()> {
        self.stop(app.clone(), &service.id).await?;
        // 等待端口释放
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        self.start(app, service).await
    }

    /// 编译并启动（自动重启流程）
    pub async fn compile_and_start(
        &self,
        app: AppHandle,
        service: Service,
    ) -> AppResult<()> {
        // 先停止
        self.stop(app.clone(), &service.id).await?;
        self.set_status(&app, &service.id, ServiceStatus::Recompiling);

        let working_dir = PathBuf::from(&service.working_dir);
        let env_cfg = resolve_env_config(&service)?;
        let (program, args) = resolve_maven_cmd(&working_dir, &env_cfg);

        let sid = service.id.clone();
        let app_clone = app.clone();
        let program = program.to_string();
        let args = args.to_vec();
        let env_cfg_for_compile = env_cfg.clone();

        // 同步执行 mvn compile
        let compile_result = tokio::task::spawn_blocking(move || {
            let mut cmd = std::process::Command::new(&program);
            cmd.args(&args);
            cmd.arg("compile");
            cmd.current_dir(&working_dir);
            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());
            cmd.stdin(std::process::Stdio::null());
            // 注入环境变量（与启动一致）
            inject_env_std(&mut cmd, &env_cfg_for_compile);
            let output = cmd.output();
            output
        })
        .await
        .map_err(|e| crate::error::AppError::Process(format!("编译任务失败: {}", e)))?;

        match compile_result {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                for line in stdout.lines().chain(stderr.lines()) {
                    Self::emit_log(&app_clone, &sid, LogSource::Mvn, line);
                }
                if output.status.success() {
                    self.start(app_clone, service).await
                } else {
                    let _cfg = db::load_config().unwrap_or_default();
                    Self::emit_log(
                        &app_clone,
                        &sid,
                        LogSource::Mvn,
                        "[javaboot] 编译失败",
                    );
                    self.set_status(&app_clone, &sid, ServiceStatus::Error);
                    Err(crate::error::AppError::Process("编译失败".into()))
                }
            }
            Err(e) => {
                self.set_status(&app_clone, &sid, ServiceStatus::Error);
                Err(crate::error::AppError::Process(format!(
                    "编译执行失败: {}",
                    e
                )))
            }
        }
    }

    /// 停止所有运行中的服务
    pub async fn stop_all(&self, app: AppHandle) -> AppResult<()> {
        let ids: Vec<String> = self.handles.lock().keys().cloned().collect();
        let count = ids.len();
        if count == 0 {
            return Ok(());
        }
        Self::emit_log_static(&app, &ids[0], "[javaboot]", &format!("[javaboot] 正在停止全部 {} 个服务...", count));
        for id in ids {
            self.stop(app.clone(), &id).await?;
        }
        Ok(())
    }

    pub fn is_running(&self, service_id: &str) -> bool {
        self.handles.lock().contains_key(service_id)
    }
}

#[derive(Clone, serde::Serialize)]
struct LogLinePayload {
    service_id: String,
    source: String,
    line: String,
    ts: String,
}

/// 底层日志推送：写事件到前端
fn emit_log_raw(app: &AppHandle, service_id: &str, tag: &str, line: &str) {
    // 清理 ANSI 颜色码（Maven/SpringBoot 会输出 \x1b[...m）
    let cleaned = strip_ansi_codes(line);
    let payload = LogLinePayload {
        service_id: service_id.to_string(),
        source: tag.to_string(),
        line: cleaned,
        ts: Utc::now().to_rfc3339(),
    };
    let _ = app.emit("service://log", payload);
}

/// 移除 ANSI 转义码（颜色、光标控制等）和多余回车
fn strip_ansi_codes(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // 跳过 ESC[ ... m 之类的序列
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&cc) = chars.peek() {
                    chars.next();
                    if cc.is_alphabetic() {
                        break;
                    }
                }
                continue;
            }
            // 其他 ESC 序列跳过
            continue;
        }
        if c == '\r' {
            continue; // 移除回车，避免 Windows 换行双字符
        }
        result.push(c);
    }
    result
}

/// 探测服务的 mainClass：
/// 1. 先查 pom.xml 里 spring-boot-maven-plugin 的 mainClass 配置
/// 2. 约定：扫描 src/main/java 下的 @SpringBootApplication 类
/// 3. 兜底：从服务名推断（如 scs-auth → 包名 com.*.scs.auth.Application）
fn detect_main_class(service: &Service, working_dir: &std::path::Path) -> String {
    // 1. 尝试从 pom.xml 解析 spring-boot-maven-plugin 的 mainClass
    let pom_path = working_dir.join("pom.xml");
    if let Ok(content) = std::fs::read_to_string(&pom_path) {
        // 简单字符串匹配找 <mainClass>xxx</mainClass>
        if let Some(start) = content.find("<mainClass>") {
            if let Some(end) = content[start..].find("</mainClass>") {
                let mc = &content[start + 11..start + end];
                if !mc.trim().is_empty() {
                    return mc.trim().to_string();
                }
            }
        }
    }

    // 2. 扫描 src/main/java 找带 @SpringBootApplication 的类
    let src_java = working_dir.join("src").join("main").join("java");
    if src_java.exists() {
        if let Some(mc) = scan_spring_application(&src_java) {
            return mc;
        }
    }

    // 3. 兜底：从服务名推断（scs-auth → 通常有 Application 结尾的类）
    // 返回一个常见的默认值，让 java 报 ClassNotFoundException 时用户能看到
    format!("{}.Application", service.name.replace('-', "."))
}

/// 递归扫描 java 文件找 @SpringBootApplication 注解
fn scan_spring_application(dir: &std::path::Path) -> Option<String> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(mc) = scan_spring_application(&path) {
                return Some(mc);
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("java") {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if content.contains("@SpringBootApplication") {
                    // 从文件路径推全限定类名：src/main/java 之后的路径 + 文件名去 .java
                    if let Ok(rel) = path.strip_prefix(dir) {
                        let class_path = rel.to_string_lossy().replace('\\', "/").replace('/', ".");
                        let fqcn = class_path.trim_end_matches(".java").to_string();
                        return Some(fqcn);
                    }
                }
            }
        }
    }
    None
}

fn check_started(line: &str) -> bool {
    for pat in STARTED_PATTERNS {
        if line.contains(pat) {
            return true;
        }
    }
    false
}

/// 环境配置（从项目解析得出）
#[derive(Clone)]
struct EnvConfig {
    java_home: Option<String>,
    maven_home: Option<String>,
    /// 项目根路径（用于多模块 install）
    project_root: Option<String>,
}

/// 从服务的 project_id 查项目，解析出项目级 JDK / Maven 配置
fn resolve_env_config(service: &Service) -> AppResult<EnvConfig> {
    let mut cfg = EnvConfig {
        java_home: None,
        maven_home: None,
        project_root: None,
    };
    if let Some(pid) = &service.project_id {
        if let Ok(project) = crate::db::get_project(pid) {
            cfg.java_home = project.java_home.and_then(|s| {
                let t = s.trim();
                if t.is_empty() { None } else { Some(t.to_string()) }
            });
            cfg.maven_home = project.maven_home.and_then(|s| {
                let t = s.trim();
                if t.is_empty() { None } else { Some(t.to_string()) }
            });
            cfg.project_root = Some(project.root_path);
        }
    }
    Ok(cfg)
}

/// 解析 Maven 命令：优先级 项目 maven_home > 项目 mvnw.cmd > mvnw.bat > mvnw > 系统 mvn
fn resolve_maven_cmd(working_dir: &std::path::Path, cfg: &EnvConfig) -> (String, Vec<String>) {
    // 1. 项目配置了 maven_home → 用它的 bin/mvn.cmd
    if let Some(mh) = &cfg.maven_home {
        let mvn_cmd = std::path::PathBuf::from(mh).join("bin").join("mvn.cmd");
        if mvn_cmd.exists() {
            return (
                "cmd".to_string(),
                vec!["/c".to_string(), mvn_cmd.to_string_lossy().to_string()],
            );
        }
        let mvn_bin = std::path::PathBuf::from(mh).join("bin").join("mvn");
        if mvn_bin.exists() {
            return (mvn_bin.to_string_lossy().to_string(), vec![]);
        }
        log::warn!("项目配置的 maven_home 无效: {}", mh);
    }
    // 2. 项目自带 mvnw
    let mvnw_cmd = working_dir.join("mvnw.cmd");
    let mvnw_bat = working_dir.join("mvnw.bat");
    let mvnw = working_dir.join("mvnw");
    if mvnw_cmd.exists() {
        ("cmd".to_string(), vec!["/c".to_string(), mvnw_cmd.to_string_lossy().to_string()])
    } else if mvnw_bat.exists() {
        ("cmd".to_string(), vec!["/c".to_string(), mvnw_bat.to_string_lossy().to_string()])
    } else if mvnw.exists() {
        (mvnw.to_string_lossy().to_string(), vec![])
    } else {
        // 3. 系统 PATH 的 mvn
        ("mvn".to_string(), vec![])
    }
}

/// 确定生效的 JAVA_HOME：项目配置优先，否则用系统环境变量
fn resolve_java_home(cfg: &EnvConfig) -> Option<String> {
    cfg.java_home.clone().or_else(|| {
        std::env::var("JAVA_HOME").ok().filter(|s| !s.is_empty())
    })
}

/// 启动前预检：确认 java / mvn 可用
fn preflight_check(
    cfg: &EnvConfig,
    working_dir: &std::path::Path,
    program: &str,
) -> AppResult<()> {
    // 1. java 可执行性检查
    let java_home = resolve_java_home(cfg);
    let java_bin = if let Some(jh) = &java_home {
        std::path::PathBuf::from(jh).join("bin").join("java.exe")
    } else {
        std::path::PathBuf::from("java.exe")
    };
    let java_ok = java_home.is_some() && java_bin.exists();
    if !java_ok {
        if which_java().is_none() {
            return Err(crate::error::AppError::Process(format!(
                "未找到可用的 JDK。\n{}请在该服务所属项目的设置里指定 JDK 路径，或确保系统 JAVA_HOME / PATH 配置正确。",
                if java_home.is_some() {
                    format!("配置的 JAVA_HOME 不存在: {}\n", java_home.unwrap())
                } else {
                    "未设置 JAVA_HOME。\n".to_string()
                }
            )));
        }
    }

    // 2. mvn 可执行性检查
    if let Some(mh) = &cfg.maven_home {
        let mvn_cmd = std::path::PathBuf::from(mh).join("bin").join("mvn.cmd");
        let mvn_bin = std::path::PathBuf::from(mh).join("bin").join("mvn");
        if !mvn_cmd.exists() && !mvn_bin.exists() {
            return Err(crate::error::AppError::Process(format!(
                "项目配置的 Maven 路径无效: {}（未找到 bin/mvn.cmd）",
                mh
            )));
        }
        return Ok(());
    }
    // 未配 maven_home：检查 mvnw 或系统 mvn
    let using_mvnw = program == "cmd" || working_dir.join("mvnw").exists()
        || working_dir.join("mvnw.cmd").exists() || working_dir.join("mvnw.bat").exists();
    if !using_mvnw {
        if which_mvn().is_none() {
            return Err(crate::error::AppError::Process(
                "未找到 mvn 命令。\n请安装 Maven 并加入 PATH，或在项目根目录放置 mvnw.cmd，或在项目设置里指定 Maven 路径。".to_string(),
            ));
        }
    }
    Ok(())
}

/// 在 PATH 中查找 java
fn which_java() -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("java.exe");
        if candidate.exists() {
            return Some(candidate.to_string_lossy().to_string());
        }
    }
    None
}

/// 在 PATH 中查找 mvn
fn which_mvn() -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("mvn.cmd");
        if candidate.exists() {
            return Some(candidate.to_string_lossy().to_string());
        }
        let candidate = dir.join("mvn");
        if candidate.exists() {
            return Some(candidate.to_string_lossy().to_string());
        }
    }
    None
}

/// 为 tokio 子进程注入环境变量：覆盖 JAVA_HOME / MAVEN_HOME，确保 PATH 含对应 bin
fn inject_env(cmd: &mut Command, cfg: &EnvConfig) {
    cmd.env_clear();
    for (k, v) in std::env::vars() {
        cmd.env(k, v);
    }
    let mut path_prefix = String::new();
    if let Some(jh) = resolve_java_home(cfg) {
        cmd.env("JAVA_HOME", &jh);
        path_prefix = format!("{}\\bin;", jh);
    }
    if let Some(mh) = &cfg.maven_home {
        cmd.env("MAVEN_HOME", mh);
        cmd.env("M2_HOME", mh);
        path_prefix = format!("{}{}\\bin;", path_prefix, mh);
    }
    if !path_prefix.is_empty() {
        let cur_path = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{}{}", path_prefix, cur_path));
    }
}

/// std::process::Command 版本的环境注入（编译时用）
fn inject_env_std(cmd: &mut std::process::Command, cfg: &EnvConfig) {
    cmd.env_clear();
    for (k, v) in std::env::vars() {
        cmd.env(k, v);
    }
    let mut path_prefix = String::new();
    if let Some(jh) = resolve_java_home(cfg) {
        cmd.env("JAVA_HOME", &jh);
        path_prefix = format!("{}\\bin;", jh);
    }
    if let Some(mh) = &cfg.maven_home {
        cmd.env("MAVEN_HOME", mh);
        cmd.env("M2_HOME", mh);
        path_prefix = format!("{}{}\\bin;", path_prefix, mh);
    }
    if !path_prefix.is_empty() {
        let cur_path = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{}{}", path_prefix, cur_path));
    }
}

// ============================ 全局单例 ============================

use once_cell::sync::Lazy;

static MANAGER: Lazy<ProcessManager> = Lazy::new(ProcessManager::new);

pub fn get_manager() -> &'static ProcessManager {
    &MANAGER
}

pub fn get_manager_ref() -> &'static ProcessManager {
    &MANAGER
}
