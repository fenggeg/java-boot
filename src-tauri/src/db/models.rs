use serde::{Deserialize, Serialize};

/// 项目（Project）— 分组 + Git 单元
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub git_available: bool,
    /// 项目级 JDK 路径（覆盖系统 JAVA_HOME），None 则用系统默认
    pub java_home: Option<String>,
    /// 项目级 Maven 路径（MAVEN_HOME），None 则用 mvnw 或系统 PATH
    pub maven_home: Option<String>,
    pub created_at: String,
}

/// 服务（App）— 核心可启动单元
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Service {
    pub id: String,
    pub name: String,
    pub pom_path: String,
    pub working_dir: String,
    pub project_id: Option<String>,
    pub auto_restart: bool,
    /// 额外 Maven 参数（如 -DskipTests, -Xmx512m），空则无
    pub maven_opts: Option<String>,
    /// Spring profiles，如 "dev"
    pub profiles: Option<String>,
    /// 主类全限定名（首次启动后自动写入）
    pub main_class: Option<String>,
    /// 开发快速启动模式（JVM/Spring 优化参数）
    pub dev_mode: bool,
    /// 配置覆盖属性 JSON：`[{"key":"spring.cloud.nacos.discovery.ip","value":"192.168.1.100"}]`
    /// 启动时转成 `-Dkey=value` 注入 JVM 系统属性，Spring Boot 优先级高于 application.yml
    pub override_properties: Option<String>,
    pub created_at: String,
}

/// 服务运行时状态（不持久化，运行时计算）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStatus {
    Stopped,
    Starting,
    Running,
    Recompiling,
    Pulling,
    Error,
    Stopping,
}

impl ServiceStatus {
    pub fn label(&self) -> &'static str {
        match self {
            ServiceStatus::Stopped => "已停止",
            ServiceStatus::Starting => "启动中",
            ServiceStatus::Running => "运行中",
            ServiceStatus::Recompiling => "重新编译中",
            ServiceStatus::Pulling => "拉取中",
            ServiceStatus::Error => "异常",
            ServiceStatus::Stopping => "停止中",
        }
    }
}

/// 服务运行时信息（前端展示用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceRuntime {
    pub service_id: String,
    pub status: ServiceStatus,
    pub pid: Option<u32>,
    /// 该 PID 所有 LISTENING 端口（含 JMX/RMI/H2 等噪声端口），用于冲突检测
    pub ports: Vec<u16>,
    /// 从 Spring Boot 启动日志解析出的真实 HTTP 服务端口（前端只展示这些）
    pub service_ports: Vec<u16>,
    pub started_at: Option<String>,
    pub port_conflict: bool,
    pub conflict_with: Vec<String>,
    /// CPU 占用百分比
    pub cpu_usage: Option<f32>,
    /// 内存占用（MB）— JVM RSS，包含 heap + Metaspace + 线程栈 + 代码缓存等
    pub memory_mb: Option<f64>,
}

impl Default for ServiceRuntime {
    fn default() -> Self {
        Self {
            service_id: String::new(),
            status: ServiceStatus::Stopped,
            pid: None,
            ports: vec![],
            service_ports: vec![],
            started_at: None,
            port_conflict: false,
            conflict_with: vec![],
            cpu_usage: None,
            memory_mb: None,
        }
    }
}

/// 应用全局配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub port_refresh_interval_secs: u64,
    pub stop_on_compile_fail: bool,
    pub auto_restart_debounce_secs: u64,
    pub log_buffer_lines: usize,
    pub stop_all_on_exit: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            port_refresh_interval_secs: 2,
            stop_on_compile_fail: false,
            auto_restart_debounce_secs: 3,
            log_buffer_lines: 10000,
            stop_all_on_exit: true,
        }
    }
}

/// POM 扫描结果项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannedModule {
    pub artifact_id: String,
    pub pom_path: String,
    pub relative_path: String,
    pub packaging: String,
    pub is_service: bool, // jar/war 且可作为服务
    pub already_added: bool,
    /// 扫描期识别到的主类全限定名（有 @SpringBootApplication 才写入），用于服务落库时直填 DB，
    /// 避免启动阶段再次扫源码（大项目串行读文件头累计可达数秒）
    pub main_class: Option<String>,
    /// 声明/继承的 Java 版本需求（归一化主版本，如 "8"/"17"），用于添加时自动匹配 JDK
    pub java_version: Option<String>,
    pub children: Vec<ScannedModule>,
}
