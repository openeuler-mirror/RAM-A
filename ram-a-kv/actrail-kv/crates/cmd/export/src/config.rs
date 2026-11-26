//! 导出命令的输出路径与采集记录筛选配置。
use config_contract::{Config, RootConfig};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Serialize, Config)]
#[serde(default)]
pub(crate) struct ExportSettings {
    /// NDJSON 输出位置
    pub output: PathBuf,
    /// 精确匹配的单个会话键
    pub session_key: Option<String>,
    /// 包含的下界
    pub from: Option<String>,
    /// 不包含的上界
    pub until: Option<String>,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            output: "exported.ndjson".into(),
            session_key: None,
            from: None,
            until: None,
        }
    }
}

impl RootConfig for ExportSettings {}
