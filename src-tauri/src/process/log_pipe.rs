//! 日志分流 & 启动/失败检测
//!
//! 拆自原 manager.rs：
//! - `LogSource` / `emit_log_raw` / `strip_ansi_codes` 处理前端日志推送
//! - `check_started` / `check_failed` 用真实关键字判断 Spring Boot 启动结果
//!   （原实现里的 `Started .* in .* seconds` 是当作字符串 `contains`，永远不会命中）

use chrono::Local;
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
        // 本地时间（RFC3339 带偏移）：前端按字符串截取 HH:mm:ss 展示，
        // 用 UTC 会导致日志时间比本地慢一个时区
        ts: Local::now().to_rfc3339(),
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

/// 从 Spring Boot 启动日志里提取 HTTP 服务端口。
///
/// 覆盖的格式（Spring Boot 2.x/3.x，Tomcat/Jetty/Netty/Undertow）：
/// - `Tomcat started on port(s): 8080 (http) with context path ''`
/// - `Tomcat started on port(s): 8080 (http)`
/// - `Jetty started on port(s) 8080 (http) with context path '/'`
/// - `Netty started on port(s): 8080`
/// - `Undertow started on port(s) 8080 (http)`
///
/// 多端口场景（如 `8080, 8081`）也会全部解析出来。
pub fn extract_service_ports(line: &str) -> Vec<u16> {
    // 关键字命中后再解析，避免每行都跑正则
    let anchor = if line.contains("started on port") {
        "started on port"
    } else {
        return vec![];
    };
    let idx = match line.find(anchor) {
        Some(i) => i + anchor.len(),
        None => return vec![],
    };
    // 跳过分隔符 `:` 或空格
    let tail = line[idx..].trim_start_matches([':', ' ']);
    // 解析连续的数字端口（逗号/空格分隔，遇到非数字非分隔符停止）
    let mut ports = vec![];
    for tok in tail.split(|c: char| c == ',' || c.is_whitespace()) {
        if tok.is_empty() {
            continue;
        }
        // 取 token 开头连续数字部分
        let num: String = tok.chars().take_while(|c| c.is_ascii_digit()).collect();
        if num.is_empty() {
            break; // 遇到 "(http)" 这种非数字 token，端口列表已结束
        }
        if let Ok(p) = num.parse::<u16>() {
            if !ports.contains(&p) {
                ports.push(p);
            }
        } else {
            break;
        }
    }
    ports
}

/// 启动失败检测：Spring Boot 打印 `APPLICATION FAILED TO START` 时立刻置错
pub fn check_failed(line: &str) -> bool {
    line.contains("APPLICATION FAILED TO START")
        || line.contains("Application run failed")
        || line.contains("Error creating bean")
            && line.contains("Failed to instantiate")
}
