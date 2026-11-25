//! 将动态库失败映射为稳定状态码和可读错误详情。
use crate::abi::ErrorDetail;
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug)]
pub struct Error {
    pub status: i32,
    pub detail: ErrorDetail,
}
impl Error {
    pub fn new(status: i32, code: &str, message: impl std::fmt::Display) -> Self {
        Self {
            status,
            detail: ErrorDetail {
                code: code.into(),
                message: message.to_string(),
                field: None,
            },
        }
    }
    pub fn input(error: impl std::fmt::Display) -> Self {
        Self::new(1, "input", error)
    }
    pub fn config(error: impl std::fmt::Display) -> Self {
        Self::new(2, "config", error)
    }
    pub fn storage(error: impl std::fmt::Display) -> Self {
        Self::new(4, "storage", error)
    }
    pub fn operation(error: impl std::fmt::Display) -> Self {
        Self::new(5, "operation", error)
    }
    pub fn internal(error: impl std::fmt::Display) -> Self {
        Self::new(6, "internal", error)
    }
}
