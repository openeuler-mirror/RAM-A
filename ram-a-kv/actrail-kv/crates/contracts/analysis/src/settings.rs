//! 分析命令的配置段、时间区间和会话筛选配置。
use crate::AnalyzeConfig;
use config_contract::{Config, RootConfig};
use serde::Serialize;
use std::{num::NonZeroUsize, path::PathBuf};

#[derive(Clone, Debug, Serialize, Config)]
#[serde(default)]
pub struct AnalyzerSettings {
    /// NDJSON 输入路径
    pub input: Option<PathBuf>,
    /// 分析结果 JSON 输出路径
    pub output: PathBuf,
    /// 分析算法阈值和资源上限
    pub options: AnalyzeConfig,
    /// 会话、时间区间和最近记录筛选
    pub selection: SelectionSettings,
}

impl Default for AnalyzerSettings {
    fn default() -> Self {
        Self {
            input: None,
            output: "analysis.json".into(),
            options: AnalyzeConfig::default(),
            selection: SelectionSettings::default(),
        }
    }
}

impl RootConfig for AnalyzerSettings {}

#[derive(Clone, Debug, Default, Serialize, Config)]
#[serde(default)]
pub struct SelectionSettings {
    /// 允许的会话键集合
    pub session_keys: Vec<String>,
    /// 多个区间取并集
    pub time_ranges: Vec<TimeRangeSettings>,
    /// 按采集时间保留最新 N 条匹配记录
    pub last_n: Option<NonZeroUsize>,
}

#[derive(Clone, Debug, Default, Serialize, Config)]
#[serde(default)]
pub struct TimeRangeSettings {
    /// 包含的下界
    pub from: Option<String>,
    /// 不包含的上界
    pub until: Option<String>,
}
