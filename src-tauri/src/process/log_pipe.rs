//! 日志分流 & 启动/失败检测
//!
//! 拆自原 manager.rs：
//! - `LogSource` / `emit_log_raw` / `strip_ansi_codes` 处理前端日志推送
//! - `check_started` / `check_failed` 用真实关键字判断 Spring Boot 启动结果
//!   （原实现里的 `Started .* in .* seconds` 是当作字符串 `contains`，永远不会命中）

use chrono::Utc;
use tauri::{AppHandle, Emitter};

/// 日志来源标签：[app]（Java 子进程）/ [mvn]（Maven 编译期）
#[derive(Clone, Copy, PartialEq)]
pub enum LogSource {
    App,
    Mvn,
}

impl LogSource {
    pub fn tag(&self) -> &'static str {
        match self {
            LogSource::App => "[app]",
            LogSource::Mvn => "[mvn]",
        }
    }
}

#[derive(Clone, serde::Serialize)]
struct LogLinePayload {
    service_id: String,
    source: String,
    line: String,
    ts: String,
}

/// 底层日志推送：清理 ANSI 码后写事件到前端
pub fn emit_log_raw(app: &AppHandle, service_id: &str, tag: &str, line: &str) {
    let cleaned = strip_ansi_codes(line);
    let payload = LogLinePayload {
        service_id: service_id.to_string(),
        source: tag.to_string(),
        line: cleaned,
        ts: Utc::now().to_rfc3339(),
    };
    let _ = app.emit("service://log", payload);
}

/// 移除 ANSI 转义码（颜色 / 光标控制）并抹掉 `\r`，避免 Windows CRLF 双字符
pub fn strip_ansi_codes(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
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
            continue;
        }
        if c == '\r' {
            continue;
        }
        result.push(c);
    }
    result
}

/// 启动成功检测：Spring Boot 2.x / 3.x / Reactive / WebFlux 都能覆盖
///
/// 只要出现以下任一即视为已启动：
/// - `Started xxxApplication in N.NNN seconds` （标准 Spring Boot 输出）
/// - `Tomcat started on port` / `Jetty started` / `Netty started on port`
/// - `Undertow - starting server`（webflux + undertow）
///
/// 相较原实现修复了 `Started .* in .* seconds` 用 `contains` 永远不命中的问题。
pub fn check_started(line: &str) -> bool {
    // "Started XxxApplication in 5.234 seconds"
    if line.contains("Started ") && line.contains(" in ") && line.contains("second") {
        return true;
    }
    line.contains("Tomcat started on port")
        || line.contains("Jetty started on port")
        || line.contains("Netty started on port")
        || line.contains("Undertow started")
}

/// 启动失败检测：Spring Boot 打印 `APPLICATION FAILED TO START` 时立刻置错
pub fn check_failed(line: &str) -> bool {
    line.contains("APPLICATION FAILED TO START")
        || line.contains("Application run failed")
        || line.contains("Error creating bean")
            && line.contains("Failed to instantiate")
}
