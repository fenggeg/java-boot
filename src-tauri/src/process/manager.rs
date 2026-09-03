//! 进程管理器（薄壳版）
//!
//! 关键子模块：
//! - [`super::log_pipe`]：日志推送 & 启动/失败检测
//! - [`super::env`]：环境解析 & 命令定位（含 PATH 探测缓存）
//! - [`super::build`]：主类探测 / classpath 缓存 / mtime 决策 / mvn 执行器
//!
//! # 锁顺序约定
//!
//! 本模块涉及三把锁，获取顺序必须严格遵循以下层级，**禁止反向加锁**以防死锁：
//! 1. `SYS`（全局 `sysinfo::System`）
//! 2. `handles`（`ProcessManager.handles`）
//! 3. `runtimes`（`ProcessManager.runtimes`）
//!
//! 典型路径：
//! - `restore_running_services`：SYS → runtimes（之后释放 SYS，再 handles → runtimes）
//! - `refresh_resource_usage`：runtimes（取快照）→ SYS → runtimes（写回）
//! - `start`：handles → runtimes（通过 set_status）
//!
//! 注意 `refresh_resource_usage` 中先短暂锁 runtimes 取快照再释放，然后锁 SYS，
//! 最后再锁 runtimes 写回——这是唯一允许的“runtimes 先于 SYS”场景，
//! 因为两次锁 runtimes 之间不持有 SYS，不构成反向加锁。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use once_cell::sync::Lazy;
use parking_lot::Mutex as PMutex;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::db;
use crate::db::models::{Service, ServiceRuntime, ServiceStatus};
use crate::error::{AppError, AppResult};
use crate::util::NoWindow;

use super::build::{
    common_mvn_flags, decide_build_strategy, detect_main_class,
    run_mvn_offline_first, run_mvn_capture, strip_verbatim_prefix,
    BuildStrategy, ClasspathCache, CompilePidSlot,
};
use super::env::{
    detect_java_major_version, inject_env, preflight_check, resolve_env_config,
    resolve_java_home, resolve_maven_cmd, EnvConfig,
};
use super::job::JobObject;
use super::log_pipe::{check_failed, check_started, emit_log_raw, extract_service_ports, LogSource};

/// Java classpath 路径分隔符：Windows 用 `;`，Unix 用 `:`
#[cfg(windows)]
const CP_SEP: &str = ";";
#[cfg(not(windows))]
const CP_SEP: &str = ":";

/// 进程管理超时常量（秒/毫秒）
/// stale placeholder 判定超时：超过此时间的 placeholder 视为残留
const STALE_PLACEHOLDER_SECS: u64 = 300;
/// stop 时等待 PID 退出超时
const STOP_WAIT_PID_SECS: u64 = 8;
/// kill 后等待进程退出超时
const KILL_WAIT_SECS: u64 = 10;
/// 依赖服务启动等待超时（秒）
const DEPENDENCY_START_TIMEOUT_SECS: u64 = 120;
/// P4：委托 daemon 停止的等待上限（秒）。daemon 最坏约 20s，这里多留余量避免误回退。
const STOP_DELEGATE_TIMEOUT_SECS: u64 = 25;
/// 轮询间隔
const POLL_INTERVAL_MS: u64 = 250;
const POLL_INTERVAL_FAST_MS: u64 = 200;
const POLL_INTERVAL_SLOW_MS: u64 = 500;

/// 按系统 ANSI 代码页编码 @argfile 内容。
///
/// java launcher 读取 @argfile 时使用系统默认编码（JDK 文档注明 "characters in
/// system default encoding"）。中文 Windows 的 ANSI 代码页是 936(GBK)，若直接写
/// UTF-8，classpath 中的中文路径会乱码。这里对 936 转 GBK，65001 保持 UTF-8，
/// 其余代码页回退 UTF-8（ASCII 兼容，非 ASCII 路径极罕见）。
fn encode_argfile(content: &str) -> Vec<u8> {
    #[cfg(windows)]
    {
        extern "system" {
            fn GetACP() -> u32;
        }
        let cp = unsafe { GetACP() };
        if cp == 936 {
            let (gbk, _, had_errors) = encoding_rs::GBK.encode(content);
            if !had_errors {
                return gbk.into_owned();
            }
        }
    }
    content.as_bytes().to_vec()
}

/// 按空白切分命令行参数，支持单/双引号包裹的含空格参数（如 `-Dfoo="a b"`）。
///
/// maven_opts 是按 mvn 风格书写的一整串参数，`split_whitespace()` 会把引号内
/// 的空格拆开导致参数被腰斩，这里保留引号语义、剔除引号本身。
fn split_args(s: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    for c in s.chars() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            c if c.is_whitespace() && !in_single && !in_double => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 不参与端口冲突判定的"噪声"端口：JMX RMI(1099)、devtools(35729)、H2 控制台(9092)、
/// JMXMP(4848) 等由框架占用、不代表服务 HTTP 端口，参与判定会产生误报。
const NOISE_PORTS: &[u16] = &[1099, 35729, 9092, 4848];

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
pub(crate) fn kill_process_tree_by_pid(pid: u32) {
    #[cfg(windows)]
    {
        let result = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags_no_window()
            .status();
        if let Err(e) = result {
            log::warn!("taskkill PID {} 失败: {}（可能进程已退出）", pid, e);
        }
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
    ///
    /// 恢复的服务会尝试重新绑定 Job Object，确保 Launcher 崩溃后再次退出时
    /// 这些进程能随之清理。若 OpenProcess 失败（权限不足等），回退到无 Job 模式。
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

        // 阶段 1：在 SYS 锁内仅采集进程存活状态，不持有 runtimes 锁
        // 缩短 SYS 锁持锁时间，避免与 refresh_resource_usage 争用
        let live_pids: Vec<(String, u32, String)> = {
            let mut sys = SYS.lock();
            sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&pid_refs), false);
            pids.iter()
                .filter(|(service_id, pid, _started_at)| {
                    let proc = sys.process(sysinfo::Pid::from_u32(*pid));
                    let is_java = proc.and_then(|p| p.name().to_str()).map_or(false, |n| {
                        n.eq_ignore_ascii_case("java.exe") || n.eq_ignore_ascii_case("javaw.exe")
                    });
                    if is_java {
                        log::info!("恢复服务 {} (PID {})", service_id, pid);
                        true
                    } else {
                        let _ = db::clear_run_pid(service_id);
                        if proc.is_some() {
                            log::warn!(
                                "服务 {} 的 PID {} 已被非 java 进程复用，清理",
                                service_id,
                                pid
                            );
                        } else {
                            log::info!("服务 {} 的进程已不存在，清理", service_id);
                        }
                        false
                    }
                })
                .cloned()
                .collect()
        };

        // 阶段 2：释放 SYS 锁后，仅持有 runtimes 锁写回
        {
            let mut rt = self.runtimes.lock();
            for (service_id, pid, started_at) in &live_pids {
                let entry = rt.entry(service_id.clone()).or_default();
                entry.service_id = service_id.clone();
                entry.status = ServiceStatus::Running;
                entry.pid = Some(*pid);
                entry.started_at = Some(started_at.clone());
                entry.ports = crate::port::ports_for_pid(*pid).unwrap_or_default()
                    .into_iter()
                    .filter(|p| !NOISE_PORTS.contains(p))
                    .collect();
            }
        }

        // 尝试为恢复的进程创建 Job Object 并绑定
        let mut handles = self.handles.lock();
        let mut restored_ids: Vec<String> = vec![];
        for (service_id, pid, _) in &pids {
            if self.runtimes.lock().get(service_id).map(|r| r.pid).flatten() != Some(*pid) {
                continue;
            }
            restored_ids.push(service_id.clone());
            let mut handle = ProcessHandle::placeholder();
            handle.pid = *pid;
            #[cfg(windows)]
            {
                match JobObject::new() {
                    Ok(job) => {
                        use windows::Win32::System::Threading::OpenProcess;
                        use windows::Win32::System::Threading::PROCESS_ACCESS_RIGHTS;
                        use windows::Win32::System::Threading::PROCESS_SET_QUOTA;
                        use windows::Win32::System::Threading::PROCESS_TERMINATE;
                        const SYNCHRONIZE: u32 = 0x00100000;
                        let access = PROCESS_SET_QUOTA | PROCESS_TERMINATE
                            | PROCESS_ACCESS_RIGHTS(SYNCHRONIZE);
                        if let Ok(ph) = unsafe { OpenProcess(access, false, *pid) } {
                            let job_arc = Arc::new(PMutex::new(job));
                            let assign_ok = job_arc.lock().assign(ph);
                            unsafe {
                                let _ = windows::Win32::Foundation::CloseHandle(ph);
                            }
                            if let Err(e) = assign_ok {
                                log::warn!("恢复服务 {} 绑定 Job Object 失败: {}", service_id, e);
                            } else {
                                handle.job = Some(job_arc);
                                log::info!("恢复服务 {} 已绑定 Job Object", service_id);
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("恢复服务 {} 创建 Job Object 失败: {}", service_id, e);
                    }
                }
            }
            handles.insert(service_id.clone(), handle);
        }

        for rt in self.all_runtimes() {
            if let Err(e) = app.emit("service://status", rt) {
                log::warn!("emit service://status 失败: {}", e);
            }
        }

        // 恢复的服务其 stdout/stderr 管道在应用重启时已断开，无法重新接管。
        // 延迟推送提示日志：setup 阶段前端 WebView 可能还没加载完、
        // listen("service://log") 尚未注册，立即 emit 会丢失。
        if !restored_ids.is_empty() {
            let app_clone = app.clone();
            let ids = restored_ids.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                for sid in &ids {
                    Self::emit_log_static(
                        &app_clone,
                        sid,
                        "[javaboot]",
                        "[javaboot] 应用重启后已恢复对该服务的托管，但日志输出管道已断开。如需查看实时日志，请重启该服务。",
                    );
                }
            });
        }
    }

    /// 刷新所有运行中服务的 CPU/内存占用
    ///
    /// 带脏检查：仅当 CPU 变化超过 0.5% 或内存变化超过 1MB 时才更新并 emit，
    /// 避免每轮定时刷新都向所有前端全量推送事件（服务多时事件风暴）。
    pub fn refresh_resource_usage(&self, app: &AppHandle) {
        // 仅采样**本地托管**的服务：daemon 托管的服务其 CPU/内存由 daemon 的
        // MonitorService 周期采样并经 proc.metrics 事件权威回填（set_metrics）。
        // 若这里也重复计算，会用本进程另采的值覆盖 daemon 的结果，造成双源抖动；
        // 且本进程首次采样（无基线）会被 clamp 到 100%，正是此前托管服务 CPU 虚高的诱因。
        let pids: Vec<(String, u32)> = {
            let rt = self.runtimes.lock();
            rt.values()
                .filter(|r| r.status == ServiceStatus::Running)
                .filter_map(|r| r.pid.map(|p| (r.service_id.clone(), p)))
                .filter(|(sid, _)| !super::delegate::is_managed(sid))
                .collect()
        };
        if pids.is_empty() {
            return;
        }
        let pid_refs: Vec<sysinfo::Pid> =
            pids.iter().map(|(_, p)| sysinfo::Pid::from_u32(*p)).collect();

        // 阶段 1：在 SYS 锁内仅采集 CPU/内存快照，不持有 runtimes 锁
        // 缩短 SYS 锁持锁时间，减少与其他调用方（wait_for_pid_exit、start）的争用
        let snapshots: Vec<(String, Option<f32>, Option<f64>)> = {
            let mut sys = SYS.lock();
            // 先刷新全局 CPU 基准：sysinfo 的进程 cpu_usage() 基于「进程 cpu 时间差分 /
            // 全局 cpu 时间差分」计算，若只刷新部分进程而不更新全局 CPU，全局时间差
            // 极小，占比会被放大到近 100%（本地托管进程持续满格的假象）。
            sys.refresh_cpu_usage();
            sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&pid_refs), true);
            pids.iter()
                .map(|(sid, pid)| {
                    let proc = sys.process(sysinfo::Pid::from_u32(*pid));
                    // sysinfo 的 cpu_usage() 在采样窗口过短或首次采样时可能瞬时超过 100%，
                    // 这里 clamp 到 [0, 100] 保证前端展示不越界。
                    let cpu = proc.map(|p| p.cpu_usage().clamp(0.0, 100.0));
                    let mem = proc.map(|p| p.memory() as f64 / 1024.0 / 1024.0);
                    (sid.clone(), cpu, mem)
                })
                .collect()
        };

        // 阶段 2：释放 SYS 锁后，仅持有 runtimes 锁写回快照
        let mut changed = false;
        {
            let mut rt = self.runtimes.lock();
            for (service_id, cpu, mem) in &snapshots {
                if cpu.is_none() {
                    continue; // 进程已退出
                }
                let entry = rt.entry(service_id.clone()).or_default();
                entry.service_id = service_id.clone();
                let cpu_changed = match entry.cpu_usage {
                    Some(old) => (cpu.unwrap_or(0.0_f32) - old).abs() > 0.5_f32,
                    None => true,
                };
                let mem_changed = match entry.memory_mb {
                    Some(old) => (mem.unwrap_or(0.0) - old).abs() > 1.0,
                    None => true,
                };
                if cpu_changed || mem_changed {
                    entry.cpu_usage = *cpu;
                    entry.memory_mb = *mem;
                    changed = true;
                }
            }
        }
        if changed {
            // 【优化】只 emit 发生变化的服务快照，而非全量推送
            let changed_runtimes: Vec<ServiceRuntime> = {
                let rt = self.runtimes.lock();
                // 取出所有有 CPU/内存数据的服务（即有变化的服务）
                rt.values()
                    .filter(|r| r.cpu_usage.is_some() || r.memory_mb.is_some())
                    .cloned()
                    .collect()
            };
            for rt in changed_runtimes {
                if let Err(e) = app.emit("service://status", rt) {
                    log::warn!("emit service://status 失败: {}", e);
                }
            }
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
                // 过滤噪声端口（JMX/DevTools/H2 等）：后端统一过滤，前端无需
                // 维护重复的 NOISE_PORTS 列表，避免前后端漂移。
                let all_ports: Vec<u16> = pid_ports.get(pid)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|p| !NOISE_PORTS.contains(p))
                    .collect();
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
            // 【优化】合并冲突刷新与状态推送为一次 emit，避免同一轮事件推送两遍
            self.refresh_port_conflicts(app);
            // refresh_port_conflicts 内部已 emit 所有 runtimes，无需再重复 emit
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
        if let Err(e) = app.emit("service://status", snapshot) {
            log::warn!("emit service://status 失败: {}", e);
        }
    }

    fn set_pid(&self, app: &AppHandle, service_id: &str, pid: u32) {
        let mut rt = self.runtimes.lock();
        let entry = rt.entry(service_id.to_string()).or_default();
        entry.service_id = service_id.to_string();
        entry.pid = Some(pid);
        entry.started_at = Some(Utc::now().to_rfc3339());
        let snapshot = entry.clone();
        drop(rt);
        if let Err(e) = app.emit("service://status", snapshot) {
            log::warn!("emit service://status 失败: {}", e);
        }
        if let Err(e) = db::save_run_pid(service_id, pid) {
            log::warn!("save_run_pid 失败 (服务 {} PID {}): {}", service_id, pid, e);
        }
    }

    /// P4：daemon 周期性回填的 CPU/内存指标，写入 runtime 并推送前端。
    pub fn set_metrics(&self, app: &AppHandle, service_id: &str, cpu: Option<f32>, mem: Option<f64>) {
        let mut rt = self.runtimes.lock();
        let entry = rt.entry(service_id.to_string()).or_default();
        entry.service_id = service_id.to_string();
        entry.cpu_usage = cpu;
        entry.memory_mb = mem;
        let snapshot = entry.clone();
        drop(rt);
        if let Err(e) = app.emit("service://status", snapshot) {
            log::warn!("emit service://status 失败: {}", e);
        }
    }

    /// 标记端口冲突
    ///
    /// 仅统计非噪声端口（过滤 JMX/devtools/H2/JMXMP 等），避免误报冲突；
    /// `ports` 字段仍保留全量端口供前端展示。
    pub fn refresh_port_conflicts(&self, app: &AppHandle) {
        let mut rt = self.runtimes.lock();
        let mut port_owners: HashMap<u16, Vec<String>> = HashMap::new();
        for r in rt.values() {
            for p in r.ports.iter().filter(|p| !NOISE_PORTS.contains(p)) {
                port_owners.entry(*p).or_default().push(r.service_id.clone());
            }
        }
        for r in rt.values_mut() {
            let mut conflicts: Vec<String> = vec![];
            for p in r.ports.iter().filter(|p| !NOISE_PORTS.contains(p)) {
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
            if let Err(e) = app.emit("service://status", s) {
                log::warn!("emit service://status 失败: {}", e);
            }
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
        // 一次性获取 handles 锁做判断，避免两把锁非原子竞态
        if self.handles.lock().contains_key(service_id) {
            return true;
        }
        // 不再单独获取 runtimes 锁做第二次判断，
        // handles 中不存在但 runtime 状态非 Stopped 的情况（如刚 stop 但事件未到）
        // 由调用方通过 retry 或 get_runtime 兜底处理
        let rt = self.runtimes.lock();
        matches!(
            rt.get(service_id).map(|r| r.status),
            Some(ServiceStatus::Running) | Some(ServiceStatus::Starting) | Some(ServiceStatus::Recompiling)
        )
    }

    fn mark_running(&self, app: &AppHandle, service_id: &str) {
        // 已是 Running 则跳过，避免日志中多次出现 "started" 关键字时反复 set_status + emit
        if self.get_runtime(service_id).status == ServiceStatus::Running {
            return;
        }
        self.set_status(app, service_id, ServiceStatus::Running);
    }

    /// 设置从 Spring Boot 启动日志解析出的 HTTP 服务端口
    ///
    /// 前端会优先展示 `service_ports`；为空时回退到 `ports`（PID 所有 LISTENING 端口）。
    /// 仅在端口列表实际发生变化时才 emit 事件，避免启动高峰期大量克隆和事件风暴。
    fn set_service_ports(&self, app: &AppHandle, service_id: &str, ports: Vec<u16>) {
        let mut rt = self.runtimes.lock();
        let entry = rt.entry(service_id.to_string()).or_default();
        entry.service_id = service_id.to_string();
        // 【修复】整表替换而非追加，避免端口变更后残留旧端口
        let changed = entry.service_ports != ports;
        if changed {
            entry.service_ports = ports;
            let snapshot = entry.clone();
            drop(rt);
            if let Err(e) = app.emit("service://status", snapshot) {
                log::warn!("emit service://status 失败: {}", e);
            }
        }
    }

    // ================================================================
    // start：核心启动流程（三档策略 + classpath 缓存 + dev_mode）
    // ================================================================

    /// 清理残留 handle 并创建 placeholder，返回 (kill_token, compile_pid)。
    ///
    /// 三种情况：
    ///   (a) pid > 0 且 sysinfo 查得到       → 真运行中，拒绝（返回 Err）
    ///   (b) pid > 0 但进程已死（僵尸）     → 静默清理后继续
    ///   (c) pid == 0（placeholder）：
    ///         - kill_token 已 signal / 超时 / runtime 已失败 → 清理
    ///         - 否则看作并发启动中 → 拒绝（返回 Err）
    fn prepare_start_placeholder(&self, service: &Service) -> Result<(Arc<AtomicBool>, CompilePidSlot), AppError> {
        let (kill_token, compile_pid) = {
            let mut handles = self.handles.lock();
            if let Some(existing) = handles.get(&service.id) {
                let existing_pid = existing.pid;
                let existing_kill_token = existing.kill_token.clone();
                let existing_created_at = existing.created_at;

                if existing_pid > 0 {
                    // 释放 handles 锁后再获取 SYS 锁，遵守锁顺序 SYS→handles
                    drop(handles);
                    let alive = {
                        let pid = sysinfo::Pid::from_u32(existing_pid);
                        let mut sys = SYS.lock();
                        sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), false);
                        sys.process(pid).is_some()
                    };
                    let mut handles = self.handles.lock();
                    if let Some(h) = handles.get(&service.id) {
                        if h.pid == existing_pid {
                            if alive {
                                let rt_status = self.runtimes.lock()
                                    .get(&service.id)
                                    .map(|r| r.status);
                                if matches!(rt_status, Some(ServiceStatus::Error) | Some(ServiceStatus::Stopped)) {
                                    log::warn!(
                                        "PID {} sysinfo 显示存活但 runtime 状态为 {:?}，清理残留后允许重启：{}",
                                        existing_pid, rt_status, service.id
                                    );
                                    h.kill_token.store(true, Ordering::Relaxed);
                                    handles.remove(&service.id);
                                    let _ = db::clear_run_pid(&service.id);
                                } else {
                                    return Err(AppError::ServiceRunning(service.id.clone()));
                                }
                            } else {
                                log::info!(
                                    "堆叠残留 handle(PID {}, elapsed {:?})，已自动清理：{}",
                                    h.pid,
                                    h.created_at.elapsed(),
                                    service.id
                                );
                                h.kill_token.store(true, Ordering::Relaxed);
                                handles.remove(&service.id);
                                let _ = db::clear_run_pid(&service.id);
                            }
                        }
                    }
                    let placeholder = ProcessHandle::placeholder();
                    let kt = placeholder.kill_token.clone();
                    let cp = placeholder.compile_pid.clone();
                    handles.insert(service.id.clone(), placeholder);
                    (kt, cp)
                } else {
                    // placeholder（pid==0）：不需要 SYS 锁，直接在 handles 锁内判断
                    let signaled = existing_kill_token.load(Ordering::Relaxed);
                    let stale = existing_created_at.elapsed() > std::time::Duration::from_secs(STALE_PLACEHOLDER_SECS);
                    let rt_status = self.runtimes.lock()
                        .get(&service.id)
                        .map(|r| r.status);
                    let failed = matches!(rt_status, Some(ServiceStatus::Error) | Some(ServiceStatus::Stopped));
                    let alive = !(signaled || stale || failed);
                    if alive {
                        return Err(AppError::ServiceRunning(service.id.clone()));
                    }
                    log::info!(
                        "堆叠残留 handle(PID {}, elapsed {:?})，已自动清理：{}",
                        existing.pid,
                        existing.created_at.elapsed(),
                        service.id
                    );
                    existing.kill_token.store(true, Ordering::Relaxed);
                    handles.remove(&service.id);
                    let _ = db::clear_run_pid(&service.id);
                    let placeholder = ProcessHandle::placeholder();
                    let kt = placeholder.kill_token.clone();
                    let cp = placeholder.compile_pid.clone();
                    handles.insert(service.id.clone(), placeholder);
                    (kt, cp)
                }
            } else {
                let placeholder = ProcessHandle::placeholder();
                let kt = placeholder.kill_token.clone();
                let cp = placeholder.compile_pid.clone();
                handles.insert(service.id.clone(), placeholder);
                (kt, cp)
            }
        };
        Ok((kill_token, compile_pid))
    }

    /// 构造 Java 启动参数列表（dev_mode JVM 优化、profiles、maven_opts、覆盖属性）。
    ///
    /// 返回 `(args, classpath_placeholder)` — args 中已包含 `-cp <classpath>` 和主类。
    fn build_java_args(
        &self,
        app: &AppHandle,
        service: &Service,
        classpath: &str,
        main_class: &str,
    ) -> Vec<String> {
        let mut args: Vec<String> = vec!["-Dfile.encoding=UTF-8".to_string()];
        if service.dev_mode {
            args.extend([
                "-XX:TieredStopAtLevel=1".into(),
                "-XX:+AlwaysPreTouch".into(),
                "-Dspring.jmx.enabled=false".into(),
                "-Dspring.output.ansi.enabled=never".into(),
                "-Dspring.devtools.restart.enabled=false".into(),
            ]);
            // 可选：Bean 懒加载，显著缩短 Spring 上下文启动（设置里开启）
            if db::load_config()
                .map(|c| c.dev_lazy_init)
                .unwrap_or(false)
            {
                args.push("-Dspring.main.lazy-initialization=true".into());
                Self::emit_log(
                    &app,
                    &service.id,
                    LogSource::Mvn,
                    "[javaboot] 已启用 Spring 懒加载 (lazy-initialization)",
                );
            }
        }
        if let Some(pf) = &service.profiles {
            if !pf.trim().is_empty() {
                args.push(format!("-Dspring.profiles.active={}", pf.trim()));
            }
        }
        if let Some(mo) = &service.maven_opts {
            for a in split_args(mo) {
                if a.starts_with("-D") || a.starts_with("-X") {
                    args.push(a);
                }
            }
        }
        // 配置覆盖属性：JSON → -Dkey=value，放在 maven_opts 的 -D 之后，
        // 确保用户在 UI 里配置的覆盖值优先级最高（Spring Boot 系统属性优先于 application.yml）
        let (overrides, parse_err) = parse_override_properties(&service.override_properties);
        if let Some(err_msg) = parse_err {
            Self::emit_log(
                &app,
                &service.id,
                LogSource::Mvn,
                &format!("[javaboot] 警告: {}", err_msg),
            );
        }
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
        args.push("-cp".into());
        args.push(classpath.to_string());
        args.push(main_class.to_string());
        if let Some(mo) = &service.maven_opts {
            for a in split_args(mo) {
                if !a.starts_with("-D") && !a.starts_with("-X") {
                    args.push(a);
                }
            }
        }
        args
    }

    /// Spawn java 子进程，绑定 Job Object，启动日志读取和 reaper。
    ///
    /// 包含 spawn 后的竞态修复（kill_token 检查）、Job Object 绑定、
    /// 日志 pipe 和进程退出 reaper。
    async fn spawn_and_monitor(
        &self,
        app: &AppHandle,
        service: &Service,
        mut cmd: Command,
        kill_token: Arc<AtomicBool>,
        working_dir: &Path,
        daemon_launch: Option<super::delegate::Launch>,
    ) -> AppResult<()> {
        // daemon 就绪门控：确保 daemon 已握手后再决定托管归属，避免「daemon 尚未
        // 就绪时启动服务」被静默回退到本地路径，导致同一批服务托管归属不一致。
        let daemon_ready = if daemon_launch.is_some() {
            if super::delegate::daemon_online(app) {
                true
            } else {
                // 未就绪：尝试拉起 daemon 并等待握手（有上限，超时才降级本地）
                let state = app.state::<Arc<crate::ipc::IpcState>>();
                state.inner().ensure_daemon_ready(std::time::Duration::from_secs(5)).await
            }
        } else {
            false // 无 daemon_launch（超长命令行等）本身就不走 daemon
        };

        let has_daemon_launch = daemon_launch.is_some();
        // P4：daemon 就绪且可委托时，把 java 进程整体交给 daemon 托管
        // （spawn / 管道消费 / 退出 / 就绪 / 指标均由 daemon 承担）
        if let Some(l) = daemon_launch {
            if daemon_ready {
                Self::emit_log(&app, &service.id, LogSource::Mvn, "[javaboot] 启动（daemon 托管）...");
                let (run_id, pid) = super::delegate::spawn_service(
                    app, &service.id, &l.module_name, &l.project_id,
                    l.argv, l.env_vars, l.working_dir, l.startup_port,
                )
                .await
                .map_err(|e| {
                    self.handles.lock().remove(&service.id);
                    self.set_status(app, &service.id, ServiceStatus::Stopped);
                    AppError::Process(format!("daemon 启动失败: {}", e))
                })?;
                let _ = run_id;
                if pid > 0 {
                    self.set_pid(app, &service.id, pid);
                }
                // 状态转 Running/Error 由 daemon proc.status 事件归一驱动
                return Ok(());
            }
        }
        // 走到这里是本地托管：要么无 daemon_launch（超长命令行不走 daemon），
        // 要么 daemon 未就绪降级。显式提示便于排查托管归属不一致问题。
        if has_daemon_launch && !daemon_ready {
            Self::emit_log(
                &app, &service.id, LogSource::Mvn,
                "[javaboot] daemon 未就绪，服务以本地模式运行（重启后可能需手动恢复）",
            );
        }
        Self::emit_log(&app, &service.id, LogSource::Mvn, "[javaboot] 启动 java 子进程...");
        let mut child = cmd.spawn().map_err(|e| {
            let msg = format!("启动失败: {}", e);
            Self::emit_log(&app, &service.id, LogSource::Mvn, &format!("[javaboot] {}", msg));
            self.handles.lock().remove(&service.id);
            self.set_status(&app, &service.id, ServiceStatus::Stopped);
            AppError::Process(format!(
                "{}\n请检查该服务配置的 JDK 路径和 Maven 是否可用。\n（若 classpath 过长，已自动切换 @argfile (JDK≥9) 或 CLASSPATH 环境变量 (JDK<9)；若仍失败请检查日志中的启动命令）",
                msg
            ))
        })?;
        let pid = child.id().unwrap_or(0);

        // 竞态修复：spawn 成功后立即将 PID 写入 handle
        if pid > 0 {
            let mut handles = self.handles.lock();
            if let Some(h) = handles.get_mut(&service.id) {
                h.pid = pid;
            }
            if kill_token.load(Ordering::Relaxed) {
                drop(handles);
                kill_process_tree_by_pid(pid);
                let _ = db::clear_run_pid(&service.id);
                self.set_status(&app, &service.id, ServiceStatus::Stopped);
                return Err(AppError::Other("启动已被停止中断".to_string()));
            }
        }

        let job = JobObject::new().map_err(|e| {
            kill_token.store(true, Ordering::Relaxed);
            kill_process_tree_by_pid(pid);
            self.handles.lock().remove(&service.id);
            self.set_status(&app, &service.id, ServiceStatus::Stopped);
            let _ = db::clear_run_pid(&service.id);
            AppError::Process(format!("Job Object 创建失败: {}", e))
        })?;
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::HANDLE;
            if let Some(h) = child.raw_handle() {
                if let Err(e) = job.assign(HANDLE(h)) {
                    log::warn!("Job assign 失败 (pid={}): {}，stop 将兜底用 kill_process_tree_by_pid", pid, e);
                    self.set_pid(&app, &service.id, pid);
                    let stdout = child.stdout.take();
                    let stderr = child.stderr.take();
                    Self::spawn_log_reader(app.clone(), service.id.clone(), stdout, stderr);
                    let app_c = app.clone();
                    let sid_c = service.id.clone();
                    let pid_c = pid;
                    tokio::spawn(async move {
                        let _ = child.wait().await;
                        log::info!("进程 {} ({}): assign 失败后的 reaper 回收退出", sid_c, pid_c);
                        let mgr = get_manager();
                        mgr.handles.lock().remove(&sid_c);
                        let _ = db::clear_run_pid(&sid_c);
                        mgr.set_status(&app_c, &sid_c, ServiceStatus::Stopped);
                    });
                    return Ok(());
                }
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
        let working_dir3 = working_dir.to_path_buf();
        tokio::spawn(async move {
            let status = child.wait().await;
            let killed = kill_token2.load(Ordering::Relaxed);
            if !killed {
                match status {
                    Ok(s) => {
                        Self::emit_log(
                            &app3, &sid3, LogSource::App,
                            &format!("[javaboot] 进程退出，退出码: {:?}", s.code()),
                        );
                        if !s.success() {
                            tokio::time::sleep(std::time::Duration::from_millis(POLL_INTERVAL_SLOW_MS)).await;
                            let argfile_path = working_dir3
                                .join("target").join(".javaboot-args.txt");
                            if argfile_path.exists() {
                                Self::emit_log(
                                    &app3, &sid3, LogSource::Mvn,
                                    &format!("[javaboot] argfile 路径: {}", argfile_path.display()),
                                );
                                if let Ok(content) = std::fs::read_to_string(&argfile_path) {
                                    let lines: Vec<&str> = content.lines().collect();
                                    Self::emit_log(
                                        &app3, &sid3, LogSource::Mvn,
                                        &format!("[javaboot] argfile 共 {} 行，前 5 行:", lines.len()),
                                    );
                                    for (i, l) in lines.iter().take(5).enumerate() {
                                        let preview = if l.len() > 200 { format!("{}...(共{}字符)", &l[..200], l.len()) } else { l.to_string() };
                                        Self::emit_log(&app3, &sid3, LogSource::Mvn, &format!("  [{}] {}", i, preview));
                                    }
                                }
                            }
                            Self::emit_log(
                                &app3, &sid3, LogSource::Mvn,
                                "[javaboot] 启动失败。可能原因：classpath 缓存过期、argfile 编码/路径问题、主类不存在或端口被占用。可尝试「重新编译并启动」刷新 classpath。",
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

    pub async fn start(&self, app: AppHandle, service: Service) -> AppResult<()> {
        let (kill_token, compile_pid) = self.prepare_start_placeholder(&service)?;

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

        // 输出实际生效的 JAVA_HOME，便于排查 "JAVA_HOME is not defined correctly" 类问题
        match resolve_java_home(&env_cfg) {
            Some(jh) => Self::emit_log(
                &app,
                &service.id,
                LogSource::Mvn,
                &format!("[javaboot] JAVA_HOME: {}", jh),
            ),
            None => Self::emit_log(
                &app,
                &service.id,
                LogSource::Mvn,
                "[javaboot] 警告: 未找到有效的 JAVA_HOME（项目配置与系统环境变量均无效）",
            ),
        }

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

        // 冷启动优化：编译与 classpath 解析合并为单次 Maven JVM，
        // 省掉第二次 JVM 启动 + 依赖图重复解析（约 1.5~3.5s）；失败降级两段式
        let mut merged_cp: Option<String> = None;
        if strategy != BuildStrategy::Skip {
            if cache_valid {
                try_cleanup!(
                    self.run_maven_build(
                        &app, &service, &env_cfg, &working_dir, &program, &base_args,
                        &compile_pid, strategy, false,
                    ).await
                );
            } else {
                match self
                    .build_and_resolve_classpath(
                        &app, &service, &env_cfg, &working_dir, &program, &base_args,
                        &compile_pid, &strategy, &cache, &cache_key,
                    )
                    .await
                {
                    Ok(cp) => merged_cp = Some(cp),
                    Err(e) => {
                        Self::emit_log(
                            &app,
                            &service.id,
                            LogSource::Mvn,
                            &format!(
                                "[javaboot] 合并编译+classpath 失败({})，降级为两段式...",
                                e
                            ),
                        );
                        try_cleanup!(
                            self.run_maven_build(
                                &app, &service, &env_cfg, &working_dir, &program, &base_args,
                                &compile_pid, strategy, false,
                            ).await
                        );
                    }
                }
            }
        }
        check_cancel!();

        // 编译已就绪：记录模块干净标记，后续启动可跳过全树 mtime 扫描
        crate::watcher::get_watch_manager()
            .mark_module_clean(&working_dir.to_string_lossy());

        let classpath = if let Some(cp) = merged_cp {
            Self::assemble_classpath(&working_dir, &env_cfg, &cp)
        } else if cache_valid {
            match cache.load() {
                Some(cp) => {
                    let jars = cp.split(CP_SEP).filter(|s| !s.is_empty()).count();
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

        // 构造 Java 参数（dev_mode、profiles、maven_opts、覆盖属性、-cp、主类）
        let args = self.build_java_args(&app, &service, &classpath, &main_class);

        // 估算命令行长度：java_bin + 各 arg + 分隔符
        let cmd_len = java_bin.len() + 1 + args.iter().map(|a| a.len() + 1).sum::<usize>();
        // Windows CreateProcessW 命令行上限 32767 字符；留余量给 quoting/program
        let over_limit = cmd_len > 30000;

        // 检测 Java 主版本：@argfile 是 JDK 9 引入的功能（JEP 294），JDK 8 不支持
        let java_major = if over_limit {
            detect_java_major_version(&env_cfg)
        } else {
            None // 不需要时不执行版本检测（有开销）
        };
        let use_argfile = over_limit && java_major.is_some_and(|v| v >= 9);
        let use_env_classpath = over_limit && !use_argfile;

        // CLASSPATH 环境变量模式下的启动参数（供本地 cmd 与 daemon 委托复用）。
        // 提升到分支外：后续构造 daemon 载荷时仍需要这两份数据。
        let mut clp_filtered_args: Vec<String> = Vec::new();
        let mut clp_value = String::new();

        let mut cmd = Command::new(&java_bin);

        if use_env_classpath {
            // JDK < 9（或版本检测失败）：@argfile 不可用
            // 将 classpath 从命令行参数移至 CLASSPATH 环境变量，大幅缩短命令行
            //
            // args 中 "-cp" 和 classpath 是连续的两个元素（见上方 push 顺序），
            // 这里把它们从 args 中移除，改为注入环境变量
            let mut cp_value = String::new();
            let mut filtered_args: Vec<String> = Vec::with_capacity(args.len() - 2);
            let mut skip_next = false;
            for arg in &args {
                if skip_next {
                    cp_value = arg.clone();
                    skip_next = false;
                    continue;
                }
                if arg == "-cp" {
                    skip_next = true;
                    continue;
                }
                filtered_args.push(arg.clone());
            }
            clp_filtered_args = filtered_args.clone();
            clp_value = cp_value.clone();
            // 检查移除 -cp 后命令行是否仍在限制内
            let new_cmd_len = java_bin.len() + 1
                + filtered_args.iter().map(|a| a.len() + 1).sum::<usize>();
            if new_cmd_len > 30000 {
                // 极端情况：移除 classpath 后仍超长（罕见，可能有超长 -D 属性）
                Self::emit_log(
                    &app, &service.id, LogSource::Mvn,
                    &format!(
                        "[javaboot] 警告: 移除 classpath 后命令行仍 {} 字符（上限 32767），可能启动失败",
                        new_cmd_len
                    ),
                );
            }
            for arg in &filtered_args {
                cmd.arg(arg);
            }
            Self::emit_log(
                &app, &service.id, LogSource::Mvn,
                &format!(
                    "[javaboot] 命令行较长 ({} 字符)，JDK {} 不支持 @argfile，改用 CLASSPATH 环境变量 ({} 字符)",
                    cmd_len,
                    java_major.map_or("未知".to_string(), |v| v.to_string()),
                    cp_value.len()
                ),
            );
            // 注入标准环境变量后再设置 CLASSPATH（确保 inject_env 不覆盖）
            inject_env(&mut cmd, &env_cfg);
            cmd.env("CLASSPATH", &cp_value);
        } else if use_argfile {
            // JDK ≥ 9：classpath 太长，写入 @argfile 启动（Java 原生支持）
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
            // java launcher 用系统默认编码读取 @argfile（JDK 文档明确：
            // "characters in system default encoding"），中文 Windows 默认 GBK，
            // 直接写 UTF-8 会让 classpath 中的中文路径乱码，故按 ANSI 代码页编码
            let bytes = encode_argfile(&content);
            if let Err(e) = std::fs::write(&argfile_path, &bytes) {
                self.handles.lock().remove(&service.id);
                self.set_status(&app, &service.id, ServiceStatus::Stopped);
                return Err(AppError::Process(format!(
                    "写入 argfile 失败 ({}): {}",
                    argfile_path.display(), e
                )));
            }
            // 关键：用 raw_arg 避免 std::process::Command 对含空格路径自动加引号。
            // Java launcher 解析 `@argfile` 时不识别 `"@path"` 这种带引号形式，
            // Command::arg 会把 `@C:\a b\args.txt` 转成 `"@C:\a b\args.txt"` 导致
            // Java 找不到文件（退出码 1）。这里自行处理：路径含空格时用 Java
            // 支持的 `@"path"` 形式（引号紧跟 @ 之后），并用 raw_arg 直传。
            let argfile_str = argfile_path.to_string_lossy().to_string();
            let at_arg = if argfile_str.contains(' ') || argfile_str.contains('\t') {
                format!("@\"{}\"", argfile_str)
            } else {
                format!("@{}", argfile_str)
            };
            cmd.raw_arg(at_arg);
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
        // use_env_classpath 分支已提前调用 inject_env + CLASSPATH，
        // 其他分支在这里统一注入
        if !use_env_classpath {
            inject_env(&mut cmd, &env_cfg);
        }
        #[cfg(windows)]
        {
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        }

        check_cancel!();

        // 诊断：输出完整的 java 启动命令（argfile 模式下输出 @path 和 args 摘要），
        // 便于定位"退出码 1 且 stderr 为空"的启动失败
        if use_argfile {
            let argfile_path = working_dir.join("target").join(".javaboot-args.txt");
            Self::emit_log(
                &app, &service.id, LogSource::Mvn,
                &format!("[javaboot] java {} @{}  (cwd: {})", java_bin, argfile_path.display(), working_dir.display()),
            );
        } else if use_env_classpath {
            // CLASSPATH 环境变量模式：输出不含 -cp 的参数 + CLASSPATH 长度
            let filtered: Vec<&str> = args.iter()
                .enumerate()
                .filter(|(i, a)| !(*a == "-cp" || (*i > 0 && args[*i - 1] == "-cp")))
                .map(|(_, a)| a.as_str())
                .collect();
            Self::emit_log(
                &app, &service.id, LogSource::Mvn,
                &format!("[javaboot] java {} {}  [CLASSPATH=env] (cwd: {})", java_bin, filtered.join(" "), working_dir.display()),
            );
        } else {
            Self::emit_log(
                &app, &service.id, LogSource::Mvn,
                &format!("[javaboot] java {} {}  (cwd: {})", java_bin, args.join(" "), working_dir.display()),
            );
        }

        // P4：构造可委托给 daemon 的启动载荷，保证**同一批服务托管归属一致**。
        //
        // 三种情形：
        // - 常规（未超长）：传完整 java 命令。
        // - 超长 + JDK<9（@argfile 不可用，改用 CLASSPATH 环境变量模式）：把 `-cp`
        //   从 argv 移除、CLASSPATH 并入 env_vars 后委托。该类命令行没有 @argfile
        //   引号问题，daemon 的 Command::args() 可安全托管；否则只要项目里有任一服务
        //   命令行长于 30000 字符就会被强制本地运行，出现「同时启动两个服务只有
        //   一个被托管」。
        // - 超长 + JDK>=9（@argfile 模式）：daemon 的 Command::args() 会把含空格的
        //   @argfile 路径再套一层引号，Java 不识别带引号的 @file 形式，故保持本地。
        let daemon_launch = if over_limit && !use_env_classpath {
            None
        } else if use_env_classpath {
            let mut argv = Vec::with_capacity(clp_filtered_args.len() + 1);
            argv.push(java_bin.clone());
            argv.extend(clp_filtered_args.iter().cloned());
            let mut env_vars = env_cfg.env_vars.clone();
            env_vars.push(("CLASSPATH".to_string(), clp_value.clone()));
            Some(super::delegate::Launch {
                argv,
                env_vars,
                working_dir: working_dir.to_string_lossy().to_string(),
                project_id: service.project_id.as_deref().unwrap_or("").to_string(),
                module_name: service.name.clone(),
                startup_port: None,
            })
        } else {
            let mut argv = Vec::with_capacity(args.len() + 1);
            argv.push(java_bin.clone());
            argv.extend(args.iter().cloned());
            Some(super::delegate::Launch {
                argv,
                env_vars: env_cfg.env_vars.clone(),
                working_dir: working_dir.to_string_lossy().to_string(),
                project_id: service.project_id.as_deref().unwrap_or("").to_string(),
                module_name: service.name.clone(),
                startup_port: None,
            })
        };

        self.spawn_and_monitor(
            &app, &service, cmd, kill_token, &working_dir, daemon_launch,
        ).await
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
        clean: bool,
    ) -> AppResult<()> {
        let project_root = env_cfg.project_root.clone();
        let (cwd, module_rel) = resolve_cwd_and_module(
            &working_dir,
            &project_root,
            Some(&strategy),
        );

        let mut args: Vec<String> = base_args.to_vec();
        args.extend(common_mvn_flags());
        if clean {
            args.push("clean".to_string());
        }
        args.push("compile".to_string());
        if !module_rel.is_empty() {
            args.push("-pl".into());
            args.push(module_rel.clone());
            if strategy == BuildStrategy::CompileAll {
                args.push("-am".into());
            }
        }
        let action_desc = if clean {
            if module_rel.is_empty() {
                "清理并编译当前模块"
            } else if strategy == BuildStrategy::CompileAll {
                "清理并编译当前模块+依赖模块"
            } else {
                "清理并编译当前模块"
            }
        } else if module_rel.is_empty() {
            "编译当前模块"
        } else if strategy == BuildStrategy::CompileAll {
            "编译当前模块+依赖模块"
        } else {
            "编译当前模块"
        };

        Self::emit_log(
            app,
            &service.id,
            LogSource::Mvn,
            &format!("[javaboot] {}: mvn {}", action_desc, args.join(" ")),
        );

        let program = program.to_string();
        let cwd_clone = cwd.clone();
        let env_cfg_clone = env_cfg.clone();
        let compile_pid_clone = compile_pid.clone();
        let app_clone = app.clone();
        let sid_clone = service.id.clone();

        let status = tokio::task::spawn_blocking(move || {
            run_mvn_offline_first(
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
    ///
    /// 在项目根目录用 `-pl <module> -am` 执行，让 Maven 精确解析当前模块及其上游依赖模块的
    /// classpath（含兄弟模块的 target/classes），避免无差别加入所有兄弟模块导致 Flyway 等
    /// classpath 扫描型工具冲突。
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

        // 计算执行目录和模块相对路径：有 project_root 时在根目录执行 -pl <mod> -am
        let (cwd, module_rel) = resolve_cwd_and_module(
            &working_dir,
            &env_cfg.project_root,
            None,
        );

        let mut args: Vec<String> = base_args.to_vec();
        args.push("dependency:build-classpath".into());
        args.push(format!("-Dmdep.outputFile={}", cp_file.to_string_lossy()));
        if !module_rel.is_empty() {
            args.push("-pl".into());
            args.push(module_rel.clone());
            args.push("-am".into());
        }
        args.push("--batch-mode".into());
        args.push("--no-transfer-progress".into());

        let program = program.to_string();
        let env_cfg_clone = env_cfg.clone();
        let compile_pid_clone = compile_pid.clone();
        let app_clone = app.clone();
        let sid_clone = service.id.clone();

        let status = tokio::task::spawn_blocking(move || {
            run_mvn_offline_first(
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

    /// 冷启动合并执行：单次 Maven JVM 同时完成「编译 + classpath 解析」
    ///
    /// `mvn [-pl mod -am] compile dependency:build-classpath`：
    /// 反应堆内每个模块按序先 compile 再写自己的 classpath 文件，
    /// `-Dmdep.outputFile=${project.build.directory}/...` 使各模块写到各自的 target 下，
    /// 只读当前服务模块那份，避免多模块互相覆盖。
    /// 相比两段式省掉一次 JVM 启动 + 依赖图重复解析（约 1.5~3.5s）。
    /// 任一环节失败由调用方降级为原两段式流程。
    #[allow(clippy::too_many_arguments)]
    async fn build_and_resolve_classpath(
        &self,
        app: &AppHandle,
        service: &Service,
        env_cfg: &EnvConfig,
        working_dir: &std::path::Path,
        program: &str,
        base_args: &[String],
        compile_pid: &CompilePidSlot,
        strategy: &BuildStrategy,
        cache: &ClasspathCache,
        cache_key: &str,
    ) -> AppResult<String> {
        Self::emit_log(
            app,
            &service.id,
            LogSource::Mvn,
            "[javaboot] 合并模式：编译 + 解析 classpath（单次 Maven 调用）...",
        );

        // 计算执行目录和模块相对路径（与 run_maven_build 同逻辑）
        let project_root = env_cfg.project_root.clone();
        let (cwd, module_rel) = resolve_cwd_and_module(
            &working_dir,
            &project_root,
            Some(strategy),
        );

        let mut args: Vec<String> = base_args.to_vec();
        args.extend(common_mvn_flags());
        if !module_rel.is_empty() {
            args.push("-pl".into());
            args.push(module_rel.clone());
            if *strategy == BuildStrategy::CompileAll {
                args.push("-am".into());
            }
        }
        args.push("compile".into());
        args.push("dependency:build-classpath".into());
        // 每个反应堆模块写各自 target 下的文件，最终只读当前模块那份
        args.push(format!(
            "-Dmdep.outputFile=${{project.build.directory}}/.javaboot-cp.txt"
        ));
        args.push("--batch-mode".into());
        args.push("--no-transfer-progress".into());

        Self::emit_log(
            app,
            &service.id,
            LogSource::Mvn,
            &format!("[javaboot] 合并构建: mvn {}", args.join(" ")),
        );

        let program = program.to_string();
        let env_cfg_clone = env_cfg.clone();
        let compile_pid_clone = compile_pid.clone();
        let app_clone = app.clone();
        let sid_clone = service.id.clone();

        let status = tokio::task::spawn_blocking(move || {
            run_mvn_offline_first(
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
        .map_err(|e| AppError::Process(format!("合并构建任务失败: {}", e)))?
        .map_err(|e| AppError::Process(format!("合并构建执行失败: {}", e)))?;

        if !status.success() {
            return Err(AppError::Process(format!(
                "合并构建失败（exit code: {:?}）",
                status.code()
            )));
        }

        let cp_file = working_dir.join("target").join(".javaboot-cp.txt");
        let dep_cp = std::fs::read_to_string(&cp_file)
            .unwrap_or_default()
            .trim()
            .to_string();
        if dep_cp.is_empty() {
            return Err(AppError::Process(
                "合并模式下 classpath 输出为空（可能 Maven 版本不支持 outputFile 表达式）".into(),
            ));
        }
        if let Err(e) = cache.save(&dep_cp, cache_key) {
            log::warn!("写 classpath 缓存失败: {}", e);
        } else {
            let jars = dep_cp.split(CP_SEP).filter(|s| !s.is_empty()).count();
            Self::emit_log(
                app,
                &service.id,
                LogSource::Mvn,
                &format!("[javaboot] classpath 已缓存 ({} 个依赖)", jars),
            );
        }
        Ok(dep_cp)
    }

    /// 组装完整 classpath：target/classes + jar 依赖
    ///
    /// **关键优化**：把 `dependency:build-classpath` 输出中属于**项目内模块**的本地仓库 jar
    /// 替换为该模块的 `target/classes`（如果存在）。
    ///
    /// 原因：多模块项目中，兄弟模块 `mvn install` 到本地仓库后源码再更新但没重新 install，
    /// `dependency:build-classpath` 输出的是本地仓库的**过期 jar**，而 `target/classes` 是最新编译的。
    /// IDEA 直接用 `target/classes` 所以正常，我们替换后行为与 IDEA 一致。
    ///
    /// 同时避免了之前 `collect_sibling_classes` 无差别加入所有兄弟模块导致的 Flyway 冲突——
    /// 只有 Maven 依赖解析中出现的模块才会被替换。
    fn assemble_classpath(
        working_dir: &std::path::Path,
        env_cfg: &EnvConfig,
        dep_cp: &str,
    ) -> String {
        let classes_dir = working_dir.join("target").join("classes");
        let mut parts: Vec<String> = vec![classes_dir.to_string_lossy().to_string()];

        if !dep_cp.is_empty() {
            // 扫描项目内所有有 target/classes 的模块，建立 artifactId → classes 路径 映射
            // 使用 TTL 缓存：同一项目 5 秒内多次启动服务时复用扫描结果，避免重复遍历目录
            let module_map = env_cfg.project_root
                .as_ref()
                .map(|root| get_cached_module_classes_map(std::path::Path::new(root)))
                .unwrap_or_default();

            // 预排序：按 artifact_id 长度降序，确保最长前缀优先匹配
            // 在循环外只排序一次，避免 replace_with_module_classes 每次调用都排序
            let mut sorted_entries: Vec<(&String, &String)> = module_map.iter().collect();
            sorted_entries.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

            for entry in dep_cp.split(CP_SEP) {
                if entry.is_empty() {
                    continue;
                }
                // 尝试从本地仓库 jar 路径提取 artifactId，替换为项目内 target/classes
                let replaced = replace_with_module_classes(entry, &sorted_entries);
                parts.push(replaced);
            }
        }
        parts.join(CP_SEP)
    }

    // ================================================================
    // stop / restart / compile_and_start / stop_all
    // ================================================================

    /// 等待指定 PID 的进程真正退出（轮询 sysinfo），带超时。
    ///
    /// `stop()` 发出 kill 后立即返回，但进程（尤其 JVM 带 shutdown hook）实际退出
    /// 可能需要 1~2 秒。后续操作（`mvn clean` 删 target、重启绑定端口）若不等待，
    /// 会撞上文件锁 / 端口占用。返回 true 表示进程已退出，false 表示超时仍存活。
    async fn wait_for_pid_exit(pid: u32, timeout: std::time::Duration) -> bool {
        if pid == 0 {
            return true;
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let alive = {
                let mut sys = SYS.lock();
                sys.refresh_processes(
                    sysinfo::ProcessesToUpdate::Some(&[sysinfo::Pid::from_u32(pid)]),
                    false,
                );
                sys.process(sysinfo::Pid::from_u32(pid)).is_some()
            };
            if !alive {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            // 【优化】轮询间隔从 100ms 提升到 250ms，减少全局 SYS 锁争用
            tokio::time::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS)).await;
        }
    }

    pub async fn stop(&self, app: AppHandle, service_id: &str) -> AppResult<()> {
        // P4：daemon 托管的服务，停止交给 daemon（进程/管道/退出由 daemon 承担）
        if super::delegate::daemon_online(&app) && super::delegate::is_managed(service_id) {
            Self::emit_log(&app, service_id, LogSource::Mvn, "[javaboot] 停止（daemon 托管）...");
            self.handles.lock().remove(service_id);
            self.set_status(&app, service_id, ServiceStatus::Stopping);
            // 带超时：daemon 停止最坏约 20s（8s 等退出 + 12s 端口释放探测）。
            // 超过上限或失败则回退本地强杀 PID，避免 UI 无限等待。
            let stopped = match tokio::time::timeout(
                std::time::Duration::from_secs(STOP_DELEGATE_TIMEOUT_SECS),
                super::delegate::stop_service(&app, service_id),
            )
            .await
            {
                Ok(Ok(_)) => true,
                Ok(Err(e)) => {
                    log::warn!("daemon 停止 {service_id} 失败: {e}，回退本地强杀");
                    false
                }
                Err(_) => {
                    log::warn!("daemon 停止 {service_id} 超时，回退本地强杀");
                    false
                }
            };
            if !stopped {
                if let Some(pid) = self.get_runtime(service_id).pid {
                    kill_process_tree_by_pid(pid);
                    let _ = Self::wait_for_pid_exit(
                        pid,
                        std::time::Duration::from_secs(STOP_WAIT_PID_SECS),
                    ).await;
                }
            }
            self.set_status(&app, service_id, ServiceStatus::Stopped);
            let _ = db::clear_run_pid(service_id);
            super::delegate::clear(service_id);
            Self::emit_log(&app, service_id, LogSource::Mvn, "[javaboot] 服务已停止");
            return Ok(());
        }

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
            let run_pid = h.pid;
            if let Some(job) = h.job {
                job.lock().kill();
            } else if run_pid > 0 {
                kill_process_tree_by_pid(run_pid);
            }
            // 等待 Java 进程真正退出：JVM shutdown hook 可能需要 1~2 秒，
            // 不等待会导致后续 restart/recompile 撞上端口占用 / class 文件锁
            if run_pid > 0 {
                if !Self::wait_for_pid_exit(run_pid, std::time::Duration::from_secs(STOP_WAIT_PID_SECS)).await {
                    log::warn!("stop: 等待 PID {} 退出超时，继续后续操作", run_pid);
                }
            }
            let _ = db::clear_run_pid(service_id); // clear: 失败影响小，restore 有 java 进程名校验兜底
            self.set_status(&app, service_id, ServiceStatus::Stopped);
            Self::emit_log(&app, service_id, LogSource::Mvn, "[javaboot] 服务已停止");
        } else {
            let pid = self.get_runtime(service_id).pid;
            if let Some(pid) = pid {
                self.set_status(&app, service_id, ServiceStatus::Stopping);
                kill_process_tree_by_pid(pid);
                if !Self::wait_for_pid_exit(pid, std::time::Duration::from_secs(STOP_WAIT_PID_SECS)).await {
                    log::warn!("stop: 等待 PID {} 退出超时，继续后续操作", pid);
                }
                let _ = db::clear_run_pid(service_id); // clear: 失败影响小，restore 有 java 进程名校验兜底
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
        // runtime.ports 已由后端过滤噪声端口，这里直接使用。
        // 等待业务端口被 OS 回收（TIME_WAIT 等），避免新进程 bind 失败。
        let old_ports: Vec<u16> = self.get_runtime(&service.id).ports.clone();
        self.stop(app.clone(), &service.id).await?;
        // stop() 已等待进程真正退出；这里额外等待业务端口被 OS 回收（TIME_WAIT 等）。
        // old_ports 为空（未解析到业务端口，如启动失败即重启）时无需等待端口，
        // 进程已退出即可启动；否则轮询确认端口释放，避免新进程 bind 失败。
        if !old_ports.is_empty() {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(KILL_WAIT_SECS);
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(POLL_INTERVAL_FAST_MS)).await;
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
        }
        self.start(app, service).await
    }

    /// 编译并启动：由 watcher 触发的自动重启流程
    ///
    /// 根据 `AppConfig.stop_on_compile_fail` 决定编译失败后是否保留旧进程：
    /// - `false`（默认）：先编译，编译成功再停旧进程并启动；失败则旧进程不受影响
    /// - `true`：先停旧进程再编译；失败后旧进程已停止（与原行为一致）
    pub async fn compile_and_start(&self, app: AppHandle, service: Service) -> AppResult<()> {
        let cfg = db::load_config().unwrap_or_default();
        let stop_on_fail = cfg.stop_on_compile_fail;

        if stop_on_fail {
            // 兼容旧行为：先停再编译，失败后服务保持停止
            self.stop(app.clone(), &service.id).await?;
            self.set_status(&app, &service.id, ServiceStatus::Recompiling);
            self.start(app, service).await
        } else {
            // 新行为：先编译，成功后再停旧进程并启动
            // 复用 start() 的编译逻辑，但不实际启动 java 进程
            self.set_status(&app, &service.id, ServiceStatus::Recompiling);
            match self.compile_only(&app, &service).await {
                Ok(()) => {
                    // 编译成功，停旧进程并启动
                    self.stop(app.clone(), &service.id).await?;
                    self.start(app, service).await
                }
                Err(e) => {
                    // 编译失败：检查旧进程是否真的还活着再恢复状态。
                    // 仅凭 rt.pid.is_some() 不够——进程可能在此期间自行崩溃退出，
                    // pid 尚未被后台 reaper 清理，会误恢复为 Running。
                    //
                    // 【修复】清理 compile_only 插入的 placeholder handle（pid==0），
                    // 否则残留的 placeholder 会被 start() 判定为"并发启动中"而拒绝启动。
                    {
                        let mut handles = self.handles.lock();
                        if let Some(h) = handles.get(&service.id) {
                            if h.pid == 0 {
                                h.kill_token.store(true, Ordering::Relaxed);
                                handles.remove(&service.id);
                            }
                        }
                    }
                    let rt = self.get_runtime(&service.id);
                    let restore = match rt.pid {
                        Some(pid) if pid > 0 => {
                            let alive = {
                                let mut sys = SYS.lock();
                                sys.refresh_processes(
                                    sysinfo::ProcessesToUpdate::Some(&[sysinfo::Pid::from_u32(pid)]),
                                    false,
                                );
                                sys.process(sysinfo::Pid::from_u32(pid)).is_some()
                            };
                            if alive {
                                ServiceStatus::Running
                            } else {
                                Self::emit_log(
                                    &app, &service.id, LogSource::Mvn,
                                    "[javaboot] 编译失败，且旧进程已退出",
                                );
                                ServiceStatus::Stopped
                            }
                        }
                        _ => ServiceStatus::Stopped,
                    };
                    self.set_status(&app, &service.id, restore);
                    Err(e)
                }
            }
        }
    }

    /// 仅执行编译流程（复用 start() 的编译逻辑，不启动 java 进程）
    async fn compile_only(&self, app: &AppHandle, service: &Service) -> AppResult<()> {
        use super::build::{
            decide_build_strategy, strip_verbatim_prefix, BuildStrategy, ClasspathCache,
        };
        use super::env::{preflight_check, resolve_env_config, resolve_maven_cmd};

        let working_dir = strip_verbatim_prefix(&PathBuf::from(&service.working_dir));
        let env_cfg = resolve_env_config(service)?;
        let (program, base_args) = resolve_maven_cmd(&working_dir, &env_cfg);
        preflight_check(&env_cfg, &working_dir, &program)?;

        let cache = ClasspathCache::for_module(&working_dir);
        let cache_key = ClasspathCache::compute_key(&working_dir, &env_cfg);
        let cache_valid = cache.is_valid(&cache_key);
        let strategy = decide_build_strategy(&working_dir, &env_cfg, cache_valid);

        Self::emit_log(
            app, &service.id, LogSource::Mvn,
            &format!("[javaboot] 构建策略: {:?}（classpath cache: {}）", strategy, if cache_valid { "hit" } else { "miss" }),
        );

        if strategy != BuildStrategy::Skip {
            // 从 handles 中获取或创建 compile_pid，确保 stop() 能中断编译进程
            let compile_pid = {
                let mut handles = self.handles.lock();
                let handle = handles.entry(service.id.clone()).or_insert_with(ProcessHandle::placeholder);
                handle.compile_pid.clone()
            };
            self.run_maven_build(
                app, service, &env_cfg, &working_dir, &program, &base_args,
                &compile_pid, strategy, false,
            ).await?;
        }
        Ok(())
    }

    /// 重新编译并启动：先停旧进程，强制 `mvn clean compile`，再启动
    ///
    /// 与 `compile_and_start` 的区别：
    /// - 强制 clean，清除 target 下旧编译产物
    /// - 强制走编译（忽略 classpath 缓存命中和 Skip 策略）
    /// - 先停旧进程再编译（clean 会删除 target/classes，旧进程可能持有 class 文件锁）
    ///
    /// `stop()` 会等待 Java 进程真正退出（轮询 sysinfo）再返回，确保 clean 时
    /// 不会撞上 class 文件锁冲突。
    pub async fn recompile_and_start(&self, app: AppHandle, service: Service) -> AppResult<()> {
        // 先停旧进程并等待其真正退出（clean 会删 target，避免 class 文件锁冲突）
        self.stop(app.clone(), &service.id).await?;
        self.set_status(&app, &service.id, ServiceStatus::Recompiling);

        use super::build::{strip_verbatim_prefix, BuildStrategy};
        use super::env::{preflight_check, resolve_env_config, resolve_maven_cmd};

        let working_dir = strip_verbatim_prefix(&PathBuf::from(&service.working_dir));
        let env_cfg = resolve_env_config(&service)?;
        let (program, base_args) = resolve_maven_cmd(&working_dir, &env_cfg);
        preflight_check(&env_cfg, &working_dir, &program)?;

        // clean 后 classpath 缓存必然失效，强制 CompileAll
        let strategy = BuildStrategy::CompileAll;
        Self::emit_log(
            &app, &service.id, LogSource::Mvn,
            "[javaboot] 重新编译：强制 clean compile（忽略缓存）",
        );

        // 从 handles 中获取或创建 compile_pid，确保 stop() 能中断编译进程
        let compile_pid = {
            let mut handles = self.handles.lock();
            let handle = handles.entry(service.id.clone()).or_insert_with(ProcessHandle::placeholder);
            handle.compile_pid.clone()
        };
        self.run_maven_build(
            &app, &service, &env_cfg, &working_dir, &program, &base_args,
            &compile_pid, strategy, true,
        ).await?;

        // 编译成功，启动（mvn clean 已删除 target，classpath 缓存文件也随之删除）
        // 关键：清理编译期间插入的 placeholder handle（pid==0, kill_token=false），
        // 否则 start() 会把它当作"并发启动中"的 placeholder 而拒绝启动
        // （start() 仅在 kill_token 已 signal 或创建超 5 分钟时才清理 placeholder）。
        {
            let mut handles = self.handles.lock();
            if let Some(h) = handles.get(&service.id) {
                if h.pid == 0 {
                    h.kill_token.store(true, Ordering::Relaxed);
                    handles.remove(&service.id);
                }
            }
        }
        self.start(app, service).await
    }

    /// 清理服务编译产物（mvn clean），不重新启动
    ///
    /// - 先停止运行中的服务（避免 class 文件锁冲突）
    /// - 执行 `mvn clean`，删除 target 目录
    /// - 清除 classpath 缓存
    pub async fn clean_service(&self, app: AppHandle, service: Service) -> AppResult<()> {
        // 先停止服务
        self.stop(app.clone(), &service.id).await?;
        self.set_status(&app, &service.id, ServiceStatus::Recompiling);

        use super::build::strip_verbatim_prefix;
        use super::env::{preflight_check, resolve_env_config, resolve_maven_cmd};

        let working_dir = strip_verbatim_prefix(&PathBuf::from(&service.working_dir));
        let env_cfg = resolve_env_config(&service)?;
        let (program, base_args) = resolve_maven_cmd(&working_dir, &env_cfg);
        preflight_check(&env_cfg, &working_dir, &program)?;

        // 计算执行目录和模块相对路径
        let (cwd, module_rel) = resolve_cwd_and_module(
            &working_dir,
            &env_cfg.project_root,
            None,
        );

        let mut args: Vec<String> = base_args.to_vec();
        args.extend(common_mvn_flags());
        args.push("clean".to_string());
        if !module_rel.is_empty() {
            args.push("-pl".into());
            args.push(module_rel.clone());
        }

        let action_desc = if module_rel.is_empty() {
            "清理当前模块"
        } else {
            "清理当前模块"
        };
        Self::emit_log(
            &app,
            &service.id,
            LogSource::Mvn,
            &format!("[javaboot] {}: mvn {}", action_desc, args.join(" ")),
        );

        // 获取 compile_pid 用于中断
        let compile_pid = {
            let mut handles = self.handles.lock();
            let handle = handles.entry(service.id.clone()).or_insert_with(ProcessHandle::placeholder);
            handle.compile_pid.clone()
        };

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
            // 【修复】清理 clean_service 插入的 placeholder handle（pid==0），
            // 避免残留 placeholder 阻塞后续 start()
            {
                let mut handles = self.handles.lock();
                if let Some(h) = handles.get(&service.id) {
                    if h.pid == 0 {
                        h.kill_token.store(true, Ordering::Relaxed);
                        handles.remove(&service.id);
                    }
                }
            }
            self.set_status(&app, &service.id, ServiceStatus::Error);
            return Err(AppError::Process(format!(
                "Maven clean 失败（exit code: {:?}）",
                status.code()
            )));
        }

        // 清除 classpath 缓存
        let cache = ClasspathCache::for_module(&working_dir);
        let _ = std::fs::remove_file(&cache.cp_file);
        let _ = std::fs::remove_file(&cache.key_file);

        Self::emit_log(
            &app,
            &service.id,
            LogSource::Mvn,
            "[javaboot] 清理完成",
        );
        self.set_status(&app, &service.id, ServiceStatus::Stopped);
        Ok(())
    }

    /// 打包单个服务（mvn clean package -DskipTests），生成可执行 jar
    ///
    /// - **不停止服务**：本项目用 exploded classpath（`java -cp target/classes:...`）启动，
    ///   运行中的 JVM 不锁定 fat jar，clean package 可安全执行（与 IDEA 行为一致）
    /// - 执行 `mvn clean package -pl <module> -am`，**不**带 `spring-boot.repackage.skip`，
    ///   让 spring-boot-maven-plugin 正常执行 repackage 生成可执行 fat jar
    /// - 跳过测试（-Dmaven.test.skip=true -DskipTests）加速打包
    /// - 打包完成后恢复打包前状态（运行中的服务继续运行，不自动重启）
    pub async fn package_service(&self, app: AppHandle, service: Service) -> AppResult<PackageResult> {
        // 不停止服务：本项目用 exploded classpath（java -cp target/classes:...）启动，
        // 运行中的 JVM 不锁定 fat jar，clean package 可安全执行（与 IDEA 行为一致）。
        // 记录打包前状态，打包后恢复（避免把运行中的服务误标为 Stopped）。
        let prev_status = self
            .runtimes
            .lock()
            .get(&service.id)
            .map(|r| r.status.clone())
            .unwrap_or(ServiceStatus::Stopped);
        self.set_status(&app, &service.id, ServiceStatus::Recompiling);

        use super::build::strip_verbatim_prefix;
        use super::env::{preflight_check, resolve_env_config, resolve_maven_cmd};

        let working_dir = strip_verbatim_prefix(&PathBuf::from(&service.working_dir));
        let env_cfg = resolve_env_config(&service)?;
        let (program, base_args) = resolve_maven_cmd(&working_dir, &env_cfg);
        preflight_check(&env_cfg, &working_dir, &program)?;

        // 计算执行目录和模块相对路径
        let (cwd, module_rel) = resolve_cwd_and_module(
            &working_dir,
            &env_cfg.project_root,
            None,
        );

        // 打包参数：clean package，跳过测试，**不**带 repackage.skip
        let mut args: Vec<String> = base_args.to_vec();
        // 只保留并行 + 静默进度条 + 编码相关 flag，**不**用 common_mvn_flags()
        // （common_mvn_flags 含 -Dspring-boot.repackage.skip=true，打包必须保留 repackage）
        args.push("-T".into());
        args.push("1C".into());
        args.push("--no-transfer-progress".into());
        args.push("-Dmaven.test.skip=true".into());
        args.push("-DskipTests".into());
        args.push("-Dproject.build.sourceEncoding=UTF-8".into());
        args.push("-Dresource.encoding=UTF-8".into());
        args.push("clean".to_string());
        args.push("package".to_string());
        if !module_rel.is_empty() {
            args.push("-pl".into());
            args.push(module_rel.clone());
            args.push("-am".into());
        }

        let action_desc = if module_rel.is_empty() {
            "打包当前模块"
        } else {
            "打包当前模块"
        };
        Self::emit_log(
            &app,
            &service.id,
            LogSource::Mvn,
            &format!("[javaboot] {}: mvn {}", action_desc, args.join(" ")),
        );

        // 获取 compile_pid 用于中断
        let compile_pid = {
            let mut handles = self.handles.lock();
            let handle = handles.entry(service.id.clone()).or_insert_with(ProcessHandle::placeholder);
            handle.compile_pid.clone()
        };

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
            // 清理 placeholder handle，避免残留阻塞后续 start()
            {
                let mut handles = self.handles.lock();
                if let Some(h) = handles.get(&service.id) {
                    if h.pid == 0 {
                        h.kill_token.store(true, Ordering::Relaxed);
                        handles.remove(&service.id);
                    }
                }
            }
            // 恢复打包前状态（运行中的服务继续运行，不因打包失败误标 Error）
            self.set_status(&app, &service.id, prev_status);
            return Err(AppError::Process(format!(
                "Maven package 失败（exit code: {:?}）",
                status.code()
            )));
        }

        // 清理 placeholder handle（打包成功后也要清理，否则会阻塞后续 start）
        {
            let mut handles = self.handles.lock();
            if let Some(h) = handles.get(&service.id) {
                if h.pid == 0 {
                    h.kill_token.store(true, Ordering::Relaxed);
                    handles.remove(&service.id);
                }
            }
        }

        // 清除 classpath 缓存（clean 已删 target，缓存失效）
        let cache = ClasspathCache::for_module(&working_dir);
        let _ = std::fs::remove_file(&cache.cp_file);
        let _ = std::fs::remove_file(&cache.key_file);

        Self::emit_log(
            &app,
            &service.id,
            LogSource::Mvn,
            "[javaboot] 打包完成",
        );
        // 恢复打包前状态（运行中的服务继续显示运行中，不误标为 Stopped）
        self.set_status(&app, &service.id, prev_status);

        // 扫描 target 目录找产物 jar（排除 *-sources.jar、*-javadoc.jar、original-*）
        let jar = find_package_jar(&working_dir);
        if let Some(ref j) = jar {
            Self::emit_log(
                &app,
                &service.id,
                LogSource::Mvn,
                &format!("[javaboot] 产物: {}", j.display()),
            );
        }

        Ok(PackageResult {
            jar_path: jar.as_ref().map(|p| p.to_string_lossy().to_string()),
            jar_size: jar.as_ref().and_then(|p| p.metadata().ok().map(|m| m.len())).unwrap_or(0),
        })
    }

    /// 批量打包项目下所有已添加的服务
    ///
    /// 逐个执行 `package_service`（串行，避免多模块并发打包争抢 target/资源）。
    /// 返回 `BatchPackageResult`，包含每个成功服务的 jar 路径。
    pub async fn package_project_services(
        &self,
        app: AppHandle,
        service_ids: &[String],
    ) -> AppResult<BatchPackageResult> {
        let mut result = BatchPackageResult::default();

        if service_ids.is_empty() {
            return Ok(result);
        }

        let total = service_ids.len();
        // 推送开始日志到第一个服务面板（让用户看到批量进度）
        Self::emit_log(
            &app,
            &service_ids[0],
            LogSource::Mvn,
            &format!("[javaboot] 开始批量打包 {} 个服务...", total),
        );

        for (idx, sid) in service_ids.iter().enumerate() {
            let service = match db::get_service(sid) {
                Ok(s) => s,
                Err(e) => {
                    result.failed.push((sid.clone(), format!("服务不存在: {}", e)));
                    continue;
                }
            };

            Self::emit_log(
                &app,
                sid,
                LogSource::Mvn,
                &format!("[javaboot] 批量打包 ({}/{})：{}", idx + 1, total, service.name),
            );

            match self.package_service(app.clone(), service).await {
                Ok(pkg) => {
                    result.succeeded.push((sid.clone(), pkg.jar_path));
                }
                Err(e) => {
                    result.failed.push((sid.clone(), e.to_string()));
                    // 不中止，继续下一个
                }
            }
        }

        Self::emit_log(
            &app,
            &service_ids[0],
            LogSource::Mvn,
            &format!(
                "[javaboot] 批量打包完成: {} 成功, {} 失败",
                result.succeeded.len(),
                result.failed.len()
            ),
        );

        Ok(result)
    }

    /// 停止所有运行中的服务（真正并行：每个 stop 独立 spawn 到 tokio runtime）
    pub async fn stop_all(&self, app: AppHandle) -> AppResult<()> {
        let ids: Vec<String> = self.handles.lock().keys().cloned().collect();
        let count = ids.len();
        if count == 0 {
            return Ok(());
        }
        // 推送到每个相关服务面板，让用户在每个 tab 都能看到停止原因
        let msg = format!("[javaboot] 正在停止全部 {} 个服务...", count);
        for id in &ids {
            Self::emit_log_static(&app, id, "[javaboot]", &msg);
        }
        // 用 JoinSet 真正并行 spawn，每个 stop 在独立的 tokio task 上执行，
        // 避免 join_all 在单 task 内并发 await 导致异步操作（emit 等）串行化。
        let mut set = tokio::task::JoinSet::new();
        for id in ids {
            let app = app.clone();
            set.spawn(async move { get_manager().stop(app, &id).await });
        }
        while let Some(res) = set.join_next().await {
            match res {
                Err(join_err) => log::warn!("stop_all task panicked: {}", join_err),
                Ok(Err(app_err)) => log::warn!("stop_all: {}", app_err),
                Ok(Ok(())) => {}
            }
        }
        Ok(())
    }

    /// 带依赖的启动：拓扑排序后按序启动（依赖先启动并等待 Running）
    ///
    /// 1. 从 DB 读取目标服务及其递归依赖链
    /// 2. Kahn 拓扑排序 + 循环检测
    /// 3. 按序启动，跳过已 Running 的
    /// 4. 每个启动后轮询等待变为 Running（超时 120s）
    pub async fn start_with_dependencies(
        &self,
        app: AppHandle,
        service: Service,
    ) -> AppResult<()> {
        let target_id = service.id.clone();

        // 1. 递归收集所有依赖
        let all_deps = db::list_all_dependencies()?;
        let sorted = topo_sort(&target_id, &all_deps)?;

        // 2. 按拓扑序启动
        for sid in &sorted {
            // 跳过目标服务本身（在循环结束后由调用方启动）
            if sid == &target_id {
                continue;
            }
            // 已在运行则跳过
            if self.is_running(sid) {
                Self::emit_log(
                    &app,
                    &target_id,
                    LogSource::Mvn,
                    &format!("[javaboot] 依赖服务 {} 已在运行，跳过", sid),
                );
                continue;
            }

            let dep_service = db::get_service(sid)?;
            Self::emit_log(
                &app,
                &target_id,
                LogSource::Mvn,
                &format!("[javaboot] 正在启动依赖服务: {}", dep_service.name),
            );
            // 每个依赖也递归走 start_with_dependencies，确保多层依赖被正确处理
            // 但 sorted 已经包含了完整依赖链，直接 start 即可
            self.start(app.clone(), dep_service).await?;

            // 等待依赖变为 Running（轮询 runtime status）
let deadline = std::time::Instant::now() + std::time::Duration::from_secs(DEPENDENCY_START_TIMEOUT_SECS);
                loop {
                    let rt = self.get_runtime(sid);
                if rt.status == ServiceStatus::Running {
                    break;
                }
                if rt.status == ServiceStatus::Error {
                    return Err(AppError::Process(format!(
                        "依赖服务 {} 启动失败，中止编排",
                        sid
                    )));
                }
                if std::time::Instant::now() >= deadline {
                    return Err(AppError::Process(format!(
                        "等待依赖服务 {} 启动超时（120s），中止编排",
                        sid
                    )));
                }
                tokio::time::sleep(std::time::Duration::from_millis(POLL_INTERVAL_SLOW_MS)).await;
            }
            Self::emit_log(
                &app,
                &target_id,
                LogSource::Mvn,
                &format!("[javaboot] 依赖服务 {} 已就绪", sid),
            );
        }

        // 3. 启动目标服务本身
        Self::emit_log(
            &app,
            &target_id,
            LogSource::Mvn,
            "[javaboot] 所有依赖已就绪，启动目标服务",
        );
        self.start(app, service).await
    }

    /// 批量启动多个服务（一键启动项目下所有服务）
    ///
    /// 策略：
    /// 1. 收集所有服务 ID（去重），加上它们的递归依赖，做全局拓扑排序
    /// 2. 按拓扑序逐个启动，跳过已 Running 的
    /// 3. 每个启动后等待 Running（超时 120s）
    /// 4. 单个服务失败不中止整体流程，记录错误并继续下一个
    /// 5. 返回成功/失败计数
    pub async fn start_services_batch(
        &self,
        app: AppHandle,
        service_ids: &[String],
    ) -> AppResult<BatchStartResult> {
        let mut result = BatchStartResult::default();

        // 收集所有服务的递归依赖关系
        let all_deps = db::list_all_dependencies()?;

        // 从所有目标服务出发做全局拓扑排序
        let sorted = match topo_sort_multi(service_ids, &all_deps) {
            Ok(s) => s,
            Err(e) => {
                // 有循环依赖时尽力而为：直接按原顺序启动
                Self::emit_log(
                    &app,
                    &service_ids[0],
                    LogSource::Mvn,
                    &format!("[javaboot] 警告: {}，受影响服务将按不确定顺序启动", e),
                );
                service_ids.to_vec()
            }
        };

        // 按拓扑序逐个启动
        for sid in &sorted {
            // 已在运行则跳过
            if self.is_running(sid) {
                result.skipped.push(sid.clone());
                continue;
            }

            let service = match db::get_service(sid) {
                Ok(s) => s,
                Err(e) => {
                    result.failed.push((sid.clone(), format!("服务不存在: {}", e)));
                    continue;
                }
            };

            Self::emit_log(
                &app,
                sid,
                LogSource::Mvn,
                &format!("[javaboot] 批量启动: {}", service.name),
            );

            match self.start(app.clone(), service).await {
                Ok(()) => {
                    // 等待变为 Running（或 Error）
                    let deadline = std::time::Instant::now()
+ std::time::Duration::from_secs(DEPENDENCY_START_TIMEOUT_SECS);
                        loop {
                            let rt = self.get_runtime(sid);
                        if rt.status == ServiceStatus::Running {
                            result.succeeded.push(sid.clone());
                            break;
                        }
                        if rt.status == ServiceStatus::Error {
                            result.failed.push((
                                sid.clone(),
                                "启动后进入错误状态".to_string(),
                            ));
                            break;
                        }
                        if rt.status == ServiceStatus::Stopped {
                            // 可能启动后立即退出（如端口冲突后 kill）
                            result.failed.push((
                                sid.clone(),
                                "启动后立即退出".to_string(),
                            ));
                            break;
                        }
                        if std::time::Instant::now() >= deadline {
                            result.failed.push((
                                sid.clone(),
                                "等待启动超时（120s）".to_string(),
                            ));
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(POLL_INTERVAL_SLOW_MS))
                            .await;
                    }
                }
                Err(e) => {
                    result.failed.push((sid.clone(), e.to_string()));
                    // 不中止，继续下一个
                }
            }
        }

        Self::emit_log(
            &app,
            &sorted[0].clone(),
            LogSource::Mvn,
            &format!(
                "[javaboot] 批量启动完成: {} 成功, {} 失败, {} 跳过",
                result.succeeded.len(),
                result.failed.len(),
                result.skipped.len()
            ),
        );

        Ok(result)
    }
}

/// 单个服务打包结果
#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct PackageResult {
    /// 产物 jar 绝对路径（找不到则为 None，如 packaging=pom 的聚合模块）
    pub jar_path: Option<String>,
    /// jar 文件大小（字节）
    pub jar_size: u64,
}

/// 批量打包结果
#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct BatchPackageResult {
    /// 每个成功打包的服务 → jar 路径
    pub succeeded: Vec<(String, Option<String>)>,
    /// 失败的服务 → 错误消息
    pub failed: Vec<(String, String)>,
}

/// 批量启动结果
#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct BatchStartResult {
    pub succeeded: Vec<String>,
    pub failed: Vec<(String, String)>,
    pub skipped: Vec<String>,
}

// ================================================================
// 配置覆盖属性解析
// ================================================================

/// 解析 service.override_properties（JSON）为有序 (key, value) 列表。
///
/// JSON 格式：`[{"key":"spring.cloud.nacos.discovery.ip","value":"192.168.1.100"}]`
/// 跳过 key 为空的条目。
///
/// 返回 `(overrides, parse_error)`：
/// - 解析成功：`(Vec<...>, None)`
/// - 解析失败：`(空 Vec, Some(错误消息))`，调用方应将错误推送到前端日志
fn parse_override_properties(json: &Option<String>) -> (Vec<(String, String)>, Option<String>) {
    let raw = match json.as_ref() {
        Some(s) => s.trim(),
        None => return (vec![], None),
    };
    if raw.is_empty() {
        return (vec![], None);
    }
    #[derive(serde::Deserialize)]
    struct Kv {
        key: String,
        value: String,
    }
    match serde_json::from_str::<Vec<Kv>>(raw) {
        Ok(list) => {
            let v: Vec<(String, String)> = list
                .into_iter()
                .filter(|kv| !kv.key.trim().is_empty())
                .map(|kv| (kv.key.trim().to_string(), kv.value))
                .collect();
            (v, None)
        }
        Err(e) => {
            let msg = format!("override_properties JSON 解析失败: {}", e);
            log::warn!("{}", msg);
            (vec![], Some(msg))
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

// ================================================================
// cwd / module_rel 计算（公共逻辑，消除 4 处重复）
// ================================================================

/// 计算 Maven 执行目录和模块相对路径。
///
/// 有 `project_root` 且（`require_strategy` 为 false 或策略为 CompileAll/CompileCurrent）时，
/// 在项目根目录执行 Maven 并通过 `-pl <module_rel> -am` 指定模块；
/// 否则直接在 `working_dir` 下执行。
///
/// 返回 `(cwd, module_rel)`：module_rel 为空表示在 cwd 根下编译，无需 -pl。
fn resolve_cwd_and_module(
    working_dir: &Path,
    project_root: &Option<String>,
    strategy: Option<&BuildStrategy>,
) -> (PathBuf, String) {
    let use_root = match (project_root, strategy) {
        (Some(root), Some(s))
            if *s == BuildStrategy::CompileAll || *s == BuildStrategy::CompileCurrent =>
        {
            Some(root)
        }
        (Some(root), None) => Some(root),
        _ => None,
    };

    match use_root {
        Some(root) => {
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
        None => (working_dir.to_path_buf(), String::new()),
    }
}

/// 扫描 `working_dir/target/` 找打包产物 jar。
///
/// 排除以下文件：
/// - `*-sources.jar` / `*-javadoc.jar`（源码/文档包，非可执行产物）
/// - `original-*`（repackage 前的原始 jar，spring-boot-maven-plugin 会保留）
/// - `.javaboot-*.txt`（classpath 缓存文件，非 jar）
///
/// 多个候选时取**文件最大**的那个（fat jar 通常比普通 jar 大）。
fn find_package_jar(working_dir: &Path) -> Option<PathBuf> {
    let target_dir = working_dir.join("target");
    let entries = std::fs::read_dir(&target_dir).ok()?;
    let mut best: Option<(PathBuf, u64)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if !name.ends_with(".jar") {
            continue;
        }
        if name.ends_with("-sources.jar") || name.ends_with("-javadoc.jar") {
            continue;
        }
        if name.starts_with("original-") {
            continue;
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if best.as_ref().map_or(true, |(_, s)| size > *s) {
            best = Some((path, size));
        }
    }
    best.map(|(p, _)| p)
}

// ================================================================
// 拓扑排序：服务依赖编排
// ================================================================

/// 对目标服务及其递归依赖做拓扑排序，返回启动顺序（依赖在前，目标在后）。
///
/// 使用 Kahn 算法（BFS）：
/// 1. 以 target 为根做 BFS，收集所有可达的依赖关系
/// 2. 统计入度，入度为 0 的先入队
/// 3. 逐个出队并降低后继入度，入度归 0 则入队
/// 4. 若最终排序数 != 节点数 → 存在循环依赖
/// 拓扑排序：从多个目标服务出发，收集相关依赖子图并排序。
/// 使用 Kahn 算法（BFS）：
/// - `targets` 为空时返回空列表
/// - 检测到循环依赖时返回 Err（调用方可选择尽力而为或中止）
fn topo_sort_multi(
    targets: &[String],
    all_deps: &[db::Dependency],
) -> AppResult<Vec<String>> {
    use std::collections::{HashMap, HashSet, VecDeque};

    if targets.is_empty() {
        return Ok(vec![]);
    }

    // 构建邻接表：dep.depends_on → dep.service_id（depends_on 在前，service_id 在后）
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    let mut nodes: HashSet<String> = HashSet::new();
    for t in targets {
        nodes.insert(t.clone());
    }

    // 从所有目标出发 BFS，收集相关依赖子图
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    for t in targets {
        queue.push_back(t.clone());
    }
    while let Some(node) = queue.pop_front() {
        if !visited.insert(node.clone()) {
            continue;
        }
        nodes.insert(node.clone());
        for dep in all_deps {
            if dep.service_id == node {
                nodes.insert(dep.depends_on.clone());
                adj.entry(dep.depends_on.clone())
                    .or_default()
                    .push(dep.service_id.clone());
                queue.push_back(dep.depends_on.clone());
            }
        }
    }

    // 计算入度
    let mut in_degree: HashMap<String, usize> = HashMap::new();
    for n in &nodes {
        in_degree.insert(n.clone(), 0);
    }
    for (_from, tos) in &adj {
        for to in tos {
            if let Some(d) = in_degree.get_mut(to) {
                *d += 1;
            }
        }
    }

    // BFS：入度 0 先入队
    let mut q: VecDeque<String> = VecDeque::new();
    for (n, &d) in &in_degree {
        if d == 0 {
            q.push_back(n.clone());
        }
    }

    let mut sorted: Vec<String> = vec![];
    while let Some(n) = q.pop_front() {
        sorted.push(n.clone());
        if let Some(succs) = adj.get(&n) {
            for s in succs {
                if let Some(d) = in_degree.get_mut(s) {
                    *d -= 1;
                    if *d == 0 {
                        q.push_back(s.clone());
                    }
                }
            }
        }
    }

    if sorted.len() != nodes.len() {
        return Err(AppError::Other(format!(
            "检测到循环依赖，涉及 {} 个服务，无法编排启动顺序",
            nodes.len() - sorted.len()
        )));
    }

    Ok(sorted)
}

/// 单目标拓扑排序的便捷封装
fn topo_sort(
    target: &str,
    all_deps: &[db::Dependency],
) -> AppResult<Vec<String>> {
    topo_sort_multi(&[target.to_string()], all_deps)
}

// ================================================================
// classpath 替换：本地仓库 jar → 项目内 target/classes
// ================================================================

/// module_classes_map 缓存 TTL：同一项目根目录 5 秒内复用扫描结果，
/// 避免批量启动多个服务时重复遍历项目目录树。
const MODULE_MAP_TTL: std::time::Duration = std::time::Duration::from_secs(5);

/// module_classes_map 缓存：`project_root → (Instant, HashMap<artifactId, classes_path>)`
static MODULE_MAP_CACHE: Lazy<PMutex<HashMap<PathBuf, (Instant, HashMap<String, String>)>>> =
    Lazy::new(|| PMutex::new(HashMap::new()));

/// 获取带 TTL 缓存的 module_classes_map：5 秒内复用，过期后重新扫描。
/// 批量启动同一项目下多个服务时，首次调用扫描目录树并缓存，后续调用直接命中缓存。
fn get_cached_module_classes_map(root: &std::path::Path) -> std::collections::HashMap<String, String> {
    let root_canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let now = Instant::now();
    {
        let cache = MODULE_MAP_CACHE.lock();
        if let Some((ts, map)) = cache.get(&root_canonical) {
            if now.duration_since(*ts) < MODULE_MAP_TTL {
                return map.clone();
            }
        }
    }
    // 缓存 miss 或过期：重新扫描并更新缓存
    let map = build_module_classes_map(root);
    let mut cache = MODULE_MAP_CACHE.lock();
    cache.insert(root_canonical, (now, map.clone()));
    map
}

/// 扫描项目根下所有含 `target/classes` 的模块目录，建立 `artifactId → classes 路径` 映射。
///
/// 扫描深度：根下一级和两级子目录（匹配 `scs-common/scs-common-core`、`wip-eims/wip-eims-api` 等结构）。
/// artifactId 取目录名（与 Maven 默认 artifactId 一致）。
fn build_module_classes_map(root: &std::path::Path) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    scan_module_dir(root, root, &mut map, 0, 2);
    map
}

fn scan_module_dir(
    root: &std::path::Path,
    dir: &std::path::Path,
    map: &mut std::collections::HashMap<String, String>,
    depth: usize,
    max_depth: usize,
) {
    if depth > max_depth {
        return;
    }
    // 当前目录有 pom.xml 且有 target/classes → 注册
    let classes = dir.join("target").join("classes");
    if dir.join("pom.xml").exists() && classes.exists() {
        let artifact_id = dir.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if !artifact_id.is_empty() {
            map.insert(artifact_id, classes.to_string_lossy().to_string());
        }
    }
    // 递归子目录
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                // 跳过 target、node_modules、.git 等
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    if name == "target" || name == "node_modules" || name.starts_with('.') {
                        continue;
                    }
                }
                scan_module_dir(root, &p, map, depth + 1, max_depth);
            }
        }
    }
}

/// 尝试把本地仓库 jar 路径替换为项目内模块的 `target/classes`。
///
/// jar 路径格式：`D:\repository\com\gyyjy\wip-eims-api\1.0-SNAPSHOT\wip-eims-api-1.0-SNAPSHOT.jar`
/// 提取文件名 `wip-eims-api-1.0-SNAPSHOT.jar`，去掉版本号后得到 artifactId `wip-eims-api`。
/// 如果映射表中有该 artifactId，返回对应的 `target/classes` 路径；否则返回原路径。
///
/// `sorted_entries` 需按 artifact_id 长度降序排序，确保最长前缀优先匹配。
/// 否则当存在前缀关系（如 "foo" 和 "foo-bar"）时，HashMap 迭代顺序不确定，
/// 可能将 "foo-bar-1.0.jar" 错误匹配到 "foo" 的 classes 目录。
fn replace_with_module_classes(
    jar_path: &str,
    sorted_entries: &[(&String, &String)],
) -> String {
    // 只处理 .jar 路径
    if !jar_path.to_lowercase().ends_with(".jar") {
        return jar_path.to_string();
    }
    // 提取文件名（不含扩展名）
    let file_name = std::path::Path::new(jar_path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    if file_name.is_empty() {
        return jar_path.to_string();
    }
    // 文件名格式：artifactId-version[-classifier]
    // artifactId 本身可能含 '-'（如 wip-eims-api），所以用映射表匹配：
    // 遍历已排序的 entries，找 file_name 以 "key-" 开头的
    for (artifact_id, classes_path) in sorted_entries {
        if file_name == **artifact_id || file_name.starts_with(&format!("{}-", artifact_id)) {
            return classes_path.to_string();
        }
    }
    jar_path.to_string()
}
