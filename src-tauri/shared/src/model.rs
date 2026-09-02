//! 运行事实数据模型（daemon 持久化 / 跨进程传输共用）。
//!
//! 说明：`argv` 与 `jvm_args` 在 P0 阶段按「一次 spawn 的完整命令列表」存于
//! `proto.process_spec.jvm_args`（JSON Array），`argv` 是其强类型视图。
//! P1 引入编译流程后，`classpath_key` / `main_class` 各自归位，语义不变。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// spawn 请求：由 launcher 侧构造，经 `proc.spawn` 交给 daemon。
/// `env_vars` 在此为明文（实际用于启动)，daemon 持久化 `process_spec` 前会脱敏。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnRequest {
    pub project_id: String,
    pub module_name: String,
    pub main_class: Option<String>,
    pub classpath_key: Option<String>,
    /// 完整启动命令（含 java.exe 与全部参数）。P0 直接整段执行。
    pub argv: Vec<String>,
    /// 明文环境变量。持久化前脱敏。
    pub env_vars: BTreeMap<String, String>,
    pub working_dir: String,
    pub dev_mode: bool,
    pub auto_restart: bool,
    /// 就绪判定用的应用端口（R5）。None 则退化为正则兜底。
    pub startup_port: Option<u16>,
}

/// `process_spec` 表行：run 的**完整可重放启动上下文**，env_vars 已脱敏。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessSpec {
    pub run_id: i64,
    pub project_id: String,
    pub module_name: String,
    pub main_class: Option<String>,
    pub classpath_key: Option<String>,
    /// JSON Array 序列化（保存完整 argv）。
    pub jvm_args: String,
    /// JSON Object 序列化，敏感键值已替换为 `«redacted»`。
    pub env_vars: String,
    pub working_dir: String,
    pub dev_mode: bool,
    pub auto_restart: bool,
    pub log_file: String,
    pub launcher_version: String,
    pub startup_port: Option<u16>,
    pub created_at: i64,
}

impl ProcessSpec {
    /// 由 spawn 请求 + 分配的 run_id 合成可持久化的 spec（环境变量已脱敏）。
    pub fn from_request(req: &SpawnRequest, run_id: i64, launcher_version: String) -> Self {
        let env_json = crate::redact::redact_map(req.env_vars.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        Self {
            run_id,
            project_id: req.project_id.clone(),
            module_name: req.module_name.clone(),
            main_class: req.main_class.clone(),
            classpath_key: req.classpath_key.clone(),
            jvm_args: serde_json::to_string(&req.argv).unwrap_or_else(|_| "[]".to_string()),
            env_vars: serde_json::to_string(&env_json).unwrap_or_else(|_| "{}".to_string()),
            working_dir: req.working_dir.clone(),
            dev_mode: req.dev_mode,
            auto_restart: req.auto_restart,
            log_file: String::new(), // spawn 成功后由 daemon 回填
            launcher_version,
            startup_port: req.startup_port,
            created_at: chrono_now_ms(),
        }
    }

    /// 反解析出用于重放/模糊匹配的命令行（含可执行名占位）。
    pub fn argv(&self) -> Vec<String> {
        serde_json::from_str(&self.jvm_args).unwrap_or_default()
    }
}

/// `service_run` 表行：一次进程的一生。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceRun {
    pub id: i64,
    pub project_id: String,
    pub module_name: String,
    pub pid: Option<u32>,
    pub started_at: i64,
    pub exit_code: Option<i32>,
    pub exit_at: Option<i64>,
}

/// 日志流标识。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stream {
    Stdout,
    Stderr,
}

/// 单条结构化日志（`service_log` 行 / `log.append` 事件载荷）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogLine {
    pub run_id: i64,
    pub seq: i64,
    pub ts: i64,
    pub stream: Stream,
    pub level: Option<String>,
    pub body: String,
}

/// 进程运行时状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcStatus {
    Starting,
    Running,
    Stopping,
    Stopped,
    Error,
    /// 崩溃恢复中被枚举到的、但归属待定。
    Unknown,
}

/// `proc.list` / 对账时下发给 UI 的单个进程事实。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub run_id: i64,
    pub module_name: String,
    pub pid: Option<u32>,
    pub status: ProcStatus,
    pub started_at: Option<i64>,
    pub ports: Vec<u16>,
    pub service_ports: Vec<u16>,
    pub cpu_usage: Option<f32>,
    pub memory_mb: Option<f64>,
    /// 崩溃恢复分类附注（P1）。
    pub recovery_hint: Option<String>,
}

/// 崩溃恢复三态判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryKind {
    /// 有精确 spec（PID 存活）可接管。
    Exact,
    /// 命令行特征模糊匹配，归属待确认。
    Fuzzy,
    /// 完全未知。
    Unknown,
}

/// 崩溃恢复上报条目（`recovery.list` 载荷 / 持久化中间态）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryEntry {
    pub pid: u32,
    pub kind: RecoveryKind,
    /// 精确反查命中的 run_id（取 Over / 干净重启用）。
    pub run_id: Option<i64>,
    pub module_name: String,
    /// 命令行摘要（模糊匹配依据 / 展示用）。
    pub cmdline: String,
    /// 是否持有可重放的 spec。
    pub had_spec: bool,
    pub startup_port: Option<u16>,
}

/// 日志文件镜像目录名（工作目录下创建）。
pub const LOG_MIRROR_DIR: &str = ".javaboot";

/// 扫描产出的一个 module 节点（树形）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanModule {
    pub artifact_id: String,
    pub pom_path: String,
    pub relative_path: String,
    pub packaging: String,
    /// 打包产物可作服务启动（jar/war）且可选服务。
    pub is_service: bool,
    /// 扫描期识别到的主类全限定名（@SpringBootApplication 才写入）。
    pub main_class: Option<String>,
    /// 声明/继承的 Java 主版本（如 "8"/"17"）。
    pub java_version: Option<String>,
    pub children: Vec<ScanModule>,
}

/// 未被外层调用覆盖时的 UTC 毫秒时间戳。
fn chrono_now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn now_ms() -> i64 {
    chrono_now_ms()
}