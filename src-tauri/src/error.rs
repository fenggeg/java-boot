use thiserror::Error;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("数据库错误: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("POM 解析错误: {0}")]
    PomParse(String),

    #[error("路径不存在: {0}")]
    NotFound(String),

    #[error("服务未找到: {0}")]
    ServiceNotFound(String),

    #[error("项目未找到: {0}")]
    ProjectNotFound(String),

    #[error("服务正在运行: {0}")]
    ServiceRunning(String),

    #[error("进程错误: {0}")]
    Process(String),

    #[error("Git 错误: {0}")]
    Git(String),

    #[error("Windows API 错误: {0}")]
    Windows(String),

    #[error("{0}")]
    Other(String),
}

impl serde::Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl From<String> for AppError {
    fn from(v: String) -> Self {
        AppError::Other(v)
    }
}

impl From<&str> for AppError {
    fn from(v: &str) -> Self {
        AppError::Other(v.to_string())
    }
}
