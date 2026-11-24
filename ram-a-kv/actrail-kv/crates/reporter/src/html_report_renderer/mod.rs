//! 本文件编排统一分析结果的编译期 HTML 模板渲染。

mod format;
mod template;
mod view;

#[cfg(test)]
#[path = "../../../../tests/unit/reporter/html_report_renderer/tests.rs"]
mod tests;

use anyhow::Result;
use askama::Template;

use actrail_kv_artifacts::AnalysisResult;

use crate::locale::{Locale, LocaleSnapshot};

use template::ReportTemplate;
use view::ReportView;

pub fn render_html(result: &AnalysisResult) -> Result<String> {
    render_html_with_locale(result, LocaleSnapshot::new(Locale::ZhCn))
}

pub fn render_html_with_locale(result: &AnalysisResult, locale: LocaleSnapshot) -> Result<String> {
    let report = ReportView::build(result, locale)?;
    Ok(ReportTemplate { report: &report }.render()?)
}
