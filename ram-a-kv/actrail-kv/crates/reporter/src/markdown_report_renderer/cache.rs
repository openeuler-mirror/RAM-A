//! 本文件渲染逐请求 KV 缓存命中率、时延与口径覆盖。

use std::fmt::Write;

use actrail_kv_artifacts::{AnalysisResult, CacheMetricBasis};
use anyhow::Result;

use crate::locale::{namespace, LocaleSnapshot};

use super::format::table_code;

pub(super) fn render(
    output: &mut String,
    result: &AnalysisResult,
    locale: LocaleSnapshot,
) -> Result<()> {
    let l = locale.create(namespace::REPORT_CACHE)?;
    writeln!(output, "## {}\n", l.get("section_title")?)?;
    writeln!(output, "{}\n", l.get("intro")?)?;
    let reported = result
        .cache_metrics
        .iter()
        .filter(|metric| metric.basis == CacheMetricBasis::Reported)
        .count();
    let estimated = result.cache_metrics.len().saturating_sub(reported);
    let average = result
        .cache_metrics
        .iter()
        .map(|metric| metric.hit_rate)
        .sum::<f64>()
        / result.cache_metrics.len() as f64;
    writeln!(
        output,
        "| {} | {} | {} | {} |\n|---:|---:|---:|---:|",
        l.get("requests")?,
        l.get("average_hit")?,
        l.get("reported")?,
        l.get("estimated")?
    )?;
    writeln!(
        output,
        "| {} | {:.1}% | {} | {} |\n",
        result.cache_metrics.len(),
        average * 100.0,
        reported,
        estimated
    )?;

    writeln!(
        output,
        "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
        l.get("session")?,
        l.get("sequence")?,
        l.get("request")?,
        l.get("prompt")?,
        l.get("cached")?,
        l.get("miss")?,
        l.get("output")?,
        l.get("ttft")?,
        l.get("total")?,
        l.get("hit_rate")?,
        l.get("delta")?,
        l.get("basis")?,
        l.get("detail")?
    )?;
    output.push_str("|---|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---|\n");
    for metric in &result.cache_metrics {
        let session = match metric.session_key.as_deref() {
            Some(session) => table_code(session),
            None => l.get("no_session")?.to_string(),
        };
        let tokens = |value: Option<u32>| {
            value
                .map(|value| value.to_string())
                .unwrap_or_else(|| "—".to_string())
        };
        let latency = |value: Option<u64>| match value {
            Some(value) => l.format("milliseconds", &[("value", value.to_string())]),
            None => Ok("—".to_string()),
        };
        let delta = metric
            .hit_rate_delta
            .map(|value| {
                format!(
                    "{}{:.1}pp",
                    if value >= 0.0 { "+" } else { "" },
                    value * 100.0
                )
            })
            .unwrap_or_else(|| "—".to_string());
        let detail = match metric.basis {
            CacheMetricBasis::Reported => "—".to_string(),
            CacheMetricBasis::Estimated => l.format(
                "estimated_detail",
                &[
                    ("lcp", metric.estimated_lcp_bytes.unwrap_or(0).to_string()),
                    ("payload", metric.payload_bytes.to_string()),
                ],
            )?,
        };
        writeln!(
            output,
            "| {session} | {} | {} | {} | {} | {} | {} | {} | {} | {:.1}% | {} | {} | {} |",
            metric.sequence,
            table_code(&metric.request_id[..metric.request_id.len().min(12)]),
            tokens(metric.prompt_tokens),
            tokens(metric.cached_tokens),
            tokens(metric.miss_tokens),
            tokens(metric.output_tokens),
            latency(metric.ttft_ms)?,
            latency(metric.total_ms)?,
            metric.hit_rate * 100.0,
            delta,
            match metric.basis {
                CacheMetricBasis::Reported => l.get("basis_reported")?,
                CacheMetricBasis::Estimated => l.get("basis_estimated")?,
            },
            detail
        )?;
    }
    output.push('\n');
    Ok(())
}
