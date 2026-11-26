//! 按需筛选采集存储，并持有可直接交给报告器的内存分析结果。
use crate::{
    loader::{Instance, Output},
    Result, SdkConfig,
};
use serde::Serialize;
use serde_json::{json, value::RawValue};
use std::path::Path;

#[derive(Debug, Default, Serialize)]
pub struct Selection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_keys: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_ranges: Option<Vec<TimeRange>>,
    #[serde(skip_serializing_if = "RecordLimit::is_inherit")]
    pub last_n: RecordLimit,
}
/// 按次继承、清除或覆盖实例配置中的记录数量限制。
#[derive(Debug, Default)]
pub enum RecordLimit {
    #[default]
    Inherit,
    All,
    Last(std::num::NonZeroUsize),
}
impl RecordLimit {
    fn is_inherit(&self) -> bool {
        matches!(self, Self::Inherit)
    }
}
impl Serialize for RecordLimit {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Last(value) => value.serialize(serializer),
            Self::All | Self::Inherit => serializer.serialize_none(),
        }
    }
}
#[derive(Debug, Serialize)]
pub struct TimeRange {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
}
pub struct Analyzer {
    instance: Instance,
}
pub struct AnalyzerFactory;
impl AnalyzerFactory {
    pub fn create(config: SdkConfig) -> Result<Analyzer> {
        Ok(Analyzer {
            instance: Instance::create(config, "analyzer")?,
        })
    }
}
impl Analyzer {
    pub fn analyze(&mut self, selection: &Selection) -> Result<AnalysisOutput> {
        let output = self
            .instance
            .call(&json!({"schema_version": 1, "selection": selection}))?;
        Ok(AnalysisOutput { output })
    }
    pub fn close(self) -> Result<()> {
        self.instance.close()
    }
}
pub struct AnalysisOutput {
    pub(crate) output: Output,
}
impl AnalysisOutput {
    /// 返回完整分析数据的借用视图，未知字段不会丢失。
    pub fn json(&self) -> Result<&RawValue> {
        self.output.data()
    }
    pub fn export(&self, path: impl AsRef<Path>) -> Result<()> {
        self.output.export(path.as_ref(), None)
    }
}
