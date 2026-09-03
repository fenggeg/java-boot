//! JSON-RPC 2.0 线协议（NDJSON over Named Pipe）。
//!
//! 约定：
//! - 一个连接是一条字节流；每条 JSON-RPC 消息用 `\n` 分隔（NDJSON）。
//! - 请求：`{"jsonrpc":"2.0","id":N,"method":"...","params":{...}}`
//! - 响应：`{"jsonrpc":"2.0","id":N,"result":{...}}` 或带 `error`
//! - 通知（事件/单向）：`{"jsonrpc":"2.0","method":"...","params":{...}}`（无 id）
//!
//! 只支持整数 id；`serde_json::to_string` 产出紧凑 JSON，天然不含裸换行，可直接分行。

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::LogLine;

// ---------------------------------------------------------------------------
// 方法 / 事件名常量
// ---------------------------------------------------------------------------
pub mod method {
    pub const HELLO: &str = "daemon.hello";
    pub const PING: &str = "daemon.ping";
    pub const SPAWN: &str = "proc.spawn";
    pub const STOP: &str = "proc.stop";
    pub const LIST: &str = "proc.list";
    pub const LOG_TAIL: &str = "log.tail";
    pub const SPEC_GET: &str = "spec.get";
    // P2 扫描
    pub const SCAN_START: &str = "scan.start";
    pub const SCAN_CANCEL: &str = "scan.cancel";
    // P1 崩溃恢复
    pub const RECOVERY_LIST: &str = "recovery.list";
    pub const RECOVERY_TAKEOVER: &str = "recovery.takeover";
    pub const RECOVERY_RESTART: &str = "recovery.restart";
    pub const RECOVERY_IGNORE: &str = "recovery.ignore";
    /// 运行时重新枚举存活 java 进程并刷新待处置列表（daemon 长期运行时不自发枚举，
    /// 供 launcher 将本地启动的进程引导纳管进 daemon）。
    pub const RECOVERY_RESCAN: &str = "recovery.rescan";
}

pub mod event {
    pub const SCAN_PROGRESS: &str = "scan.progress";
    pub const SCAN_DONE: &str = "scan.done";
    pub const LOG_APPEND: &str = "log.append";
    pub const PROC_STATUS: &str = "proc.status";
    // P3 监控：周期采样 per-run CPU/内存，循环推送。
    pub const PROC_METRICS: &str = "proc.metrics";
}

/// JSON-RPC 错误码（预留标准段位）。
pub const ERR_INVALID_REQUEST: i32 = -32600;
pub const ERR_METHOD_NOT_FOUND: i32 = -32601;
pub const ERR_INVALID_PARAMS: i32 = -32602;
pub const ERR_INTERNAL_ERROR: i32 = -32603;
/// 授权/版本不兼容等业务错误，取正数避免与标准段冲突。
pub const ERR_HANDSHAKE_REQUIRED: i32 = 1001;
pub const ERR_VERSION_INCOMPATIBLE: i32 = 1002;

// ---------------------------------------------------------------------------
// 消息结构
// ---------------------------------------------------------------------------
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), data: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn ok(id: u64, result: impl Into<Value>) -> Self {
        Self { jsonrpc: "2.0".into(), id, result: Some(result.into()), error: None }
    }
    pub fn err(id: u64, e: RpcError) -> Self {
        Self { jsonrpc: "2.0".into(), id, result: None, error: Some(e) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    pub params: Option<Value>,
}

impl Notification {
    pub fn named<T: serde::Serialize>(method: &str, params: &T) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params: serde_json::to_value(params).ok(),
        }
    }
    pub fn raw(method: &str, params: Option<Value>) -> Self {
        Self { jsonrpc: "2.0".into(), method: method.into(), params }
    }
}

/// 三种消息统一定义。
#[derive(Debug, Clone)]
pub enum Message {
    Request(Request),
    Response(Response),
    Notification(Notification),
}

impl Message {
    /// 序列化为单行 JSON（NDJSON）。
    pub fn to_line(&self) -> String {
        match self {
            Message::Request(r) => serde_json::to_string(&r).unwrap_or_else(|_| "{}".into()),
            Message::Response(r) => serde_json::to_string(&r).unwrap_or_else(|_| "{}".into()),
            Message::Notification(n) => serde_json::to_string(&n).unwrap_or_else(|_| "{}".into()),
        }
    }

    /// 从单行 JSON 解析；非法行返回 `Err(说明)`。
    pub fn from_line(line: &str) -> Result<Message, String> {
        let v: Value = serde_json::from_str(line).map_err(|e| format!("JSON 解析失败: {e}"))?;
        Self::from_value(&v)
    }

    pub fn from_value(v: &Value) -> Result<Message, String> {
        let obj = v.as_object().ok_or_else(|| "消息必须是 JSON 对象".to_string())?;
        if let Some(method) = obj.get("method").and_then(|m| m.as_str()) {
            // 请求（带 id）或通知（无 id）
            match obj.get("id") {
                Some(Value::Number(n)) => {
                    let id = n.as_u64().unwrap_or(0);
                    Ok(Message::Request(Request {
                        jsonrpc: "2.0".into(),
                        id,
                        method: method.into(),
                        params: obj.get("params").cloned(),
                    }))
                }
                _ => Ok(Message::Notification(Notification {
                    jsonrpc: "2.0".into(),
                    method: method.into(),
                    params: obj.get("params").cloned(),
                })),
            }
        } else if let Some(Value::Number(n)) = obj.get("id") {
            let id = n.as_u64().unwrap_or(0);
            Ok(Message::Response(Response {
                jsonrpc: "2.0".into(),
                id,
                result: obj.get("result").cloned(),
                error: obj.get("error").cloned().and_then(|e| serde_json::from_value(e).ok()),
            }))
        } else {
            Err("消息缺少 method 或 id".to_string())
        }
    }
}

/// 递增 id 来源（客户端用于构建请求）。
pub struct IdSource(AtomicU64);

impl Default for IdSource {
    fn default() -> Self {
        Self(AtomicU64::new(1))
    }
}

impl IdSource {
    pub fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// 方法载荷
// ---------------------------------------------------------------------------

/// `daemon.hello` 请求参数（客户端 → 服务端）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloParams {
    pub client_version: String,
}

/// `daemon.hello` 返回（服务端 → 客户端）。UI 据此做版本协商。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloResult {
    pub daemon_version: String,
    pub min_client_version: String,
    pub protocol_version: u32,
    pub has_running: bool,
    pub has_pending_recovery: bool,
}

/// `proc.spawn` 返回。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnResult {
    pub run_id: i64,
    pub pid: u32,
}

/// `log.tail` 请求参数（按 run_id + seq 增量拉取）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogTailParams {
    pub run_id: i64,
    pub after_seq: i64,
    #[serde(default = "default_tail_limit")]
    pub limit: usize,
}

fn default_tail_limit() -> usize {
    2000
}

/// `log.tail` 返回。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogTailResult {
    pub next_seq: i64,
    pub entries: Vec<LogLine>,
}

/// `proc.stop` 请求参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StopParams {
    pub run_id: i64,
}

/// `spec.get` 请求参数 / 返回。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecGetParams {
    pub run_id: i64,
}

// ---------------------------------------------------------------------------
// 崩溃恢复载荷
// ---------------------------------------------------------------------------

/// `recovery.list` 返回。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryListResult {
    pub pending: Vec<crate::model::RecoveryEntry>,
}

/// `recovery.takeover/restart/ignore` 请求参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryAct {
    pub pid: u32,
}

// ---------------------------------------------------------------------------
// 扫描载荷（R4）
// ---------------------------------------------------------------------------

/// `scan.start` 参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanStartParams {
    pub project_path: String,
}

/// `scan.start` 返回：命中缓存即直接返回树（秒级）；否则后台扫描，经事件回调。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanStartResult {
    pub scan_id: String,
    pub cached: bool,
    pub tree: Vec<crate::model::ScanModule>,
}

/// `scan.cancel` 参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanCancelParams {
    pub scan_id: String,
}

/// `scan.progress` 事件（发现一个上报一个）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanProgressEvent {
    pub scan_id: String,
    pub idx: usize,
    pub module_path: String,
}

/// `scan.done` 事件（后台扫描完成）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanDoneEvent {
    pub scan_id: String,
    pub tree: Vec<crate::model::ScanModule>,
}

// ---------------------------------------------------------------------------
// 事件载荷
// ---------------------------------------------------------------------------

/// `log.append` 事件参数（服务端 → 客户端，按发现一条推一条）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogAppendEvent {
    pub line: LogLine,
}

/// `proc.metrics` 事件参数（P3 监控：单进程资源采样）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcMetrics {
    pub run_id: i64,
    #[serde(default)]
    pub cpu_usage: Option<f32>,
    #[serde(default)]
    pub memory_mb: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LogLine, Stream};

    #[test]
    fn roundtrip_messages() {
        let id = 7u64;
        let req = Message::Request(Request {
            jsonrpc: "2.0".into(),
            id,
            method: method::PING.into(),
            params: None,
        });
        let line = req.to_line();
        let back = Message::from_line(&line).unwrap();
        assert!(matches!(back, Message::Request(_)));
        if let Message::Request(r) = back {
            assert_eq!(r.id, id);
            assert_eq!(r.method, method::PING);
        }

        let ok = Message::Response(Response::ok(id, serde_json::json!({"ok": true})));
        let back = Message::from_line(&ok.to_line()).unwrap();
        assert!(matches!(back, Message::Response(_)));

        let line = LogLine {
            run_id: 3,
            seq: 12,
            ts: 1,
            stream: Stream::Stderr,
            level: None,
            body: "hello 世界".into(),
        };
        let notif = Message::Notification(Notification::named(event::LOG_APPEND, &LogAppendEvent { line }));
        let back = Message::from_line(&notif.to_line()).unwrap();
        if let Message::Notification(n) = back {
            assert_eq!(n.method, event::LOG_APPEND);
        }
    }

    #[test]
    fn rejects_empty_or_bad_objects() {
        assert!(Message::from_line("{}").is_err());
        assert!(Message::from_line("not json").is_err());
        // 带 id 即成响应（宽松解析，容忍缺失 result/error）
        assert!(matches!(Message::from_line(r#"{"id":1}"#), Ok(Message::Response(_))));
        // 字符串 id 且无 method → 视为通知（宽松）
        assert!(matches!(Message::from_line(r#"{"id":"str","method":"x"}"#), Ok(Message::Notification(_))));
    }

    #[test]
    fn ndjson_has_no_newline_inside() {
        let notif = Notification::named(event::LOG_APPEND, &LogAppendEvent {
            line: LogLine {
                run_id: 1,
                seq: 1,
                ts: 1,
                stream: Stream::Stdout,
                level: None,
                body: "多行\n文本".into(),
            },
        });
        let line = Message::Notification(notif).to_line();
        // 序列化后不应含有裸换行，便于 NDJSON 分行。
        assert!(!line.contains('\n'));
    }

    #[test]
    fn id_source_increments() {
        let s = IdSource::default();
        assert_eq!(s.next(), 1);
        assert_eq!(s.next(), 2);
    }
}