//! 本文件把统一分析结果渲染为纯 GFM Markdown 报告。

use std::fmt::Write;

use actrail_kv_artifacts::AnalysisResult;
use anyhow::Result;

use crate::locale::{namespace, Locale, LocaleSnapshot};

mod cache;
mod defects;
mod format;
mod prefix_reuse;
mod prefix_tree;
mod session;

pub fn render_markdown(result: &AnalysisResult) -> Result<String> {
    render_markdown_with_locale(result, LocaleSnapshot::new(Locale::ZhCn))
}

pub fn render_markdown_with_locale(
    result: &AnalysisResult,
    locale: LocaleSnapshot,
) -> Result<String> {
    let common = locale.create(namespace::REPORT_COMMON)?;
    let mut output = String::with_capacity(24_384);
    writeln!(output, "# {}\n", common.get("document_title")?)?;
    writeln!(
        output,
        "{}\n",
        common.format(
            "summary",
            &[
                ("input", result.run.input_records.to_string()),
                ("analyzed", result.run.analyzed_records.to_string()),
                ("skipped", result.run.skipped_records.len().to_string()),
                ("defects", result.defects.len().to_string()),
            ],
        )?
    )?;
    writeln!(output, "> {}\n", common.get("limitations")?)?;
    if let Some(summary) = &result.prefix_reuse {
        prefix_reuse::render(&mut output, summary, &result.cache_metrics, locale)?;
    }
    defects::render_top_k(&mut output, result, locale)?;
    defects::render_details(&mut output, result, locale)?;
    if !result.session_reports.is_empty() {
        session::render(&mut output, &result.session_reports, locale)?;
    }
    if !result.cache_metrics.is_empty() {
        cache::render(&mut output, result, locale)?;
    }
    if result
        .templates
        .iter()
        .any(|template| template.prefix_view.is_some())
    {
        prefix_tree::render(&mut output, result, locale)?;
    }
    Ok(output)
}

#[cfg(test)]
#[path = "../../../../tests/unit/reporter/markdown_report_renderer/tests.rs"]
mod tests;
