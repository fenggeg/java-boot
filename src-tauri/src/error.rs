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

    #[error("服务已在运行中，如需重启请先点击停止")]
    ServiceRunning(String),

    #[error("进程错误: {0}")]
    Process(String),

    #[error("Windows API 错误: {0}")]
    Windows(String),

    #[error("操作已取消: {0}")]
    Cancelled(String),

    #[error("{0}")]
    Other(String),
}

impl AppError {
    /// 稳定的机器可读错误类别（供前端按 kind 分支处理，避免解析文案）。
    pub fn kind(&self) -> &'static str {
        match self {
            AppError::Db(_) => "db",
            AppError::Io(_) => "io",
            AppError::PomParse(_) => "pom_parse",
            AppError::NotFound(_) => "not_found",
            AppError::ServiceNotFound(_) => "service_not_found",
            AppError::ProjectNotFound(_) => "project_not_found",
            AppError::ServiceRunning(_) => "service_running",
            AppError::Process(_) => "process",
            AppError::Windows(_) => "windows",
            AppError::Cancelled(_) => "cancelled",
            AppError::Other(_) => "other",
        }
    }
}

impl serde::Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("AppError", 2)?;
        state.serialize_field("kind", self.kind())?;
        state.serialize_field("message", &self.to_string())?;
        state.end()
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
