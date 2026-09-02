//! daemon 侧错误类型（库代码约定用 thiserror）。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQLite 错误: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("JSON 错误: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Windows 错误: {0}")]
    Windows(#[from] windows::core::Error),
    #[error("已停止: {0}")]
    NotRunning(String),
    #[error("非法参数: {0}")]
    Invalid(String),
    #[error("未找到: {0}")]
    NotFound(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// 透传给 JSON-RPC 错误响应（消费自身，移动语义便于 `map_err(Error::rpc)`）。
    pub fn rpc(self) -> jb_core::protocol::RpcError {
        let (code, msg) = match self {
            Error::Invalid(m) => (jb_core::protocol::ERR_INVALID_PARAMS, m.clone()),
            Error::NotFound(m) => (
                jb_core::protocol::ERR_INVALID_PARAMS,
                format!("not_found: {m}"),
            ),
            Error::NotRunning(m) => (
                jb_core::protocol::ERR_INTERNAL_ERROR,
                format!("not_running: {m}"),
            ),
            other => (
                jb_core::protocol::ERR_INTERNAL_ERROR,
                other.to_string(),
            ),
        };
        jb_core::protocol::RpcError::new(code, msg)
    }
}