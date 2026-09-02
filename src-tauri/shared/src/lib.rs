//! # jb-core
//!
//! javaboot-launcher 的 UI(Launcher) 与常驻 daemon 之间共享的**协议与数据模型**。
//!
//! 此 crate 不含任何平台相关或 IO 逻辑，只负责：
//! - 常量（管道名、版本号、心跳参数）
//! - 运行事实模型（`ProcessSpec` / `ServiceRun` / 日志行）
//! - JSON-RPC 2.0 线协议类型与编解码
//! - 敏感环境变量脱敏
//!
//! 保持零 IO、零 runtime 依赖，UI 与 daemon 两侧均可安全依赖。

pub mod consts;
pub mod model;
pub mod protocol;
pub mod redact;