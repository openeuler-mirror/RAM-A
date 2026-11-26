//! 报告输入、输出、格式和语言的文件配置入口。
use crate::ReportFormat;
use config_contract::{Config, RootConfig};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Config)]
#[serde(default)]
pub struct ReportSettings {
    /// 为空时使用 analyzer.output
    pub input: Option<PathBuf>,
    /// 为空时 Markdown 单格式使用 report.md
    pub output: Option<PathBuf>,
    /// 可选 html、md
    pub format: Option<ReportFormat>,
    /// 支持 zh-CN、en-US
    pub locale: String,
}

impl Default for ReportSettings {
    fn default() -> Self {
        Self {
            input: None,
            output: None,
            format: None,
            locale: "zh-CN".into(),
        }
    }
}

impl RootConfig for ReportSettings {}
