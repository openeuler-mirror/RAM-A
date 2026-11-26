//! 本文件公开内存或文件分析结果加载与 HTML/GFM Markdown 报告生成入口。

pub mod config;
mod html_report_renderer;
pub mod locale;
mod markdown_report_renderer;
mod report_output;
mod result_loader;

use std::path::Path;

use anyhow::Result;

pub use html_report_renderer::{render_html, render_html_with_locale};
pub use locale::{Locale, LocaleFactory, LocaleSnapshot};
pub use markdown_report_renderer::{render_markdown, render_markdown_with_locale};
pub use report_contract::ReportFormat;
pub use report_output::report_paths;
pub use result_loader::{load_result, read_result};

/// 从 analysis.json 生成一个原子写入的静态 HTML 报告。
pub fn write_report(input: &Path, output: &Path) -> Result<()> {
    report_output::write_html_only(input, output)
}

/// 按可选格式生成报告；未指定格式时同时生成 HTML 与 Markdown。
pub fn write_reports(input: &Path, output: &Path, format: Option<ReportFormat>) -> Result<()> {
    report_output::write_reports(input, output, format)
}

#[cfg(test)]
#[path = "../../../tests/unit/reporter/lib.rs"]
mod tests;
