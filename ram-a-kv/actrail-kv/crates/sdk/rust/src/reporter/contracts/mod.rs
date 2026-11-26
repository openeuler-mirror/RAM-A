//! 直接借用分析结果渲染报告，并按显式格式导出正文。
use crate::{
    analyzer::AnalysisOutput,
    loader::{Instance, Output},
    Result, SdkConfig,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;

pub use report_contract::ReportFormat;
#[derive(Debug, Default, Serialize)]
pub struct RenderOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<ReportFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
}
#[derive(Debug, Deserialize)]
pub struct ReportContent {
    pub format: ReportFormat,
    pub content: String,
}
#[derive(Deserialize)]
struct Reports {
    reports: Vec<ReportContent>,
}
pub struct Reporter {
    instance: Instance,
}
pub struct ReporterFactory;
impl ReporterFactory {
    pub fn create(config: SdkConfig) -> Result<Reporter> {
        Ok(Reporter {
            instance: Instance::create(config, "reporter")?,
        })
    }
}
impl Reporter {
    pub fn render(
        &mut self,
        analysis: &AnalysisOutput,
        options: &RenderOptions,
    ) -> Result<ReportOutput> {
        let mut request = serde_json::to_value(options)?;
        request["schema_version"] = json!(1);
        let output = self.instance.render(&analysis.output, &request)?;
        Ok(ReportOutput { output })
    }
    pub fn close(self) -> Result<()> {
        self.instance.close()
    }
}
pub struct ReportOutput {
    output: Output,
}
impl ReportOutput {
    pub fn reports(&self) -> Result<Vec<ReportContent>> {
        Ok(serde_json::from_str::<Reports>(self.output.data()?.get())?.reports)
    }
    /// 单格式结果可直接导出，多格式结果使用 export_format。
    pub fn export(&self, path: impl AsRef<Path>) -> Result<()> {
        self.output.export(path.as_ref(), None)
    }
    pub fn export_format(&self, path: impl AsRef<Path>, format: ReportFormat) -> Result<()> {
        self.output.export(path.as_ref(), Some(format.name()))
    }
}
