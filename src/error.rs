use thiserror::Error;
pub type Result<T> = std::result::Result<T, Error>;

// 对外错误只包含可公开的信息，禁止附加凭据或底层响应全文。
#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    NotFound(String),
    #[error("需要登录或登录已过期")]
    Unauthorized,
    #[error("{0}")]
    Internal(&'static str),
    #[error("{0}")]
    External(String),
    #[error("请求过于频繁，请稍后重试")]
    RateLimited(u64),
}
#[cfg(feature = "web")]
impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self {
        Self::Internal("数据库操作失败")
    }
}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self::Internal("文件操作失败")
    }
}
impl From<serde_json::Error> for Error {
    fn from(_: serde_json::Error) -> Self {
        Self::Invalid("数据格式不正确".into())
    }
}
impl Error {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}
