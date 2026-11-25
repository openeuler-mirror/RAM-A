//! 定义 SDK 配置与错误，业务配置文本保持整体透传。
use actrail_kv_sdk_contracts::ErrorDetail;
use std::path::PathBuf;
mod overrides;
pub use overrides::*;

pub struct SdkConfig {
    pub library_path: PathBuf,
    /// 内存配置和显式导出的相对路径基准，必须为绝对路径。
    pub base_dir: PathBuf,
    /// None 读取默认 settings.toml，Some(text) 使用内存 TOML（空字符串使用默认值）。
    pub config_toml: Option<String>,
    /// 只覆盖显式设置的字段，不写回配置文件。
    pub overrides: SdkOverrides,
}
#[derive(Debug, thiserror::Error)]
pub enum SdkError {
    #[error("dynamic library load failed: {0}")]
    Load(String),
    #[error("ABI mismatch: {0}")]
    Abi(String),
    #[error("invalid SDK response: {0}")]
    Protocol(String),
    #[error("operation failed ({status}, {code}): {message}", code=.detail.code, message=.detail.message)]
    Operation { status: i32, detail: ErrorDetail },
    #[error("JSON conversion failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("I/O failed: {0}")]
    Io(#[from] std::io::Error),
}
pub type Result<T> = std::result::Result<T, SdkError>;
