//! 为原生动态库提供配置、线程隔离、FFI 所有权和原子导出支持。
mod boundary;
mod config;
mod error;
mod exports;
mod output;
mod source;
mod worker;
pub use actrail_kv_sdk_contracts as abi;
pub use boundary::Boundary;
pub use config::Config;
pub use error::{Error, Result};
pub use output::publish;
pub use source::SourceProtection;
pub use tokio::runtime::Runtime;
pub use worker::{Application, Worker};

/// 解码经版本检查的 JSON 请求，业务字段由所属模块解析。
pub fn request(bytes: &[u8]) -> Result<serde_json::Value> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(Error::input)?;
    // 已批准的 ABI schema 边界，拒绝不兼容的调用。
    if value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        != Some(abi::SCHEMA_VERSION as u64)
    {
        return Err(Error::new(
            1,
            "schema_version",
            "unsupported or missing SDK schema_version",
        ));
    }
    Ok(value)
}

pub fn analysis_data(bytes: &[u8]) -> Result<serde_json::Value> {
    let mut response = request(bytes)?;
    if response.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(Error::input("expected a successful analysis response"));
    }
    response
        .get_mut("data")
        .map(serde_json::Value::take)
        .ok_or_else(|| Error::input("analysis response is missing data"))
}
