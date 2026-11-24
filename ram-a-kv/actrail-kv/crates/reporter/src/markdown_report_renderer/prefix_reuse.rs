//! 本文件渲染结构复用率、字节构成及结构代理/真实命中对照。

use std::{collections::BTreeMap, fmt::Write};

use actrail_kv_artifacts::{CacheMetricBasis, KvCacheMetric, PrefixReuseSummary};
use anyhow::Result;

use crate::locale::{namespace, LocaleSnapshot};

use super::format::{code, human_bytes};

const PREFIX_REUSE_TARGET: f64 = 0.9;
const MAX_CROSS_CHECK_ROWS: usize = 8;

pub(super) fn render(
    output: &mut String,
    summary: &PrefixReuseSummary,
    cache_metrics: &[KvCacheMetric],
    locale: LocaleSnapshot,
) -> Result<()> {
    let l = locale.create(namespace::REPORT_PREFIX_REUSE)?;
    let current = summary.ratio.clamp(0.0, 1.0);
    let potential = summary.potential_ratio.clamp(current, 1.0);
    let target = PREFIX_REUSE_TARGET.clamp(0.0, 1.0);
    writeln!(output, "## {}\n", l.get("section_title")?)?;
    writeln!(output, "{}\n", l.get("scope")?)?;
    writeln!(
        output,
        "| {} | {} |\n|---|---:|",
        l.get("metric_header")?,
        l.get("value_header")?
    )?;
    writeln!(
        output,
        "| {} | {:.1}% |",
        l.get("current")?,
        current * 100.0
    )?;
    writeln!(output, "| {} | {:.0}% |", l.get("target")?, target * 100.0)?;
    writeln!(
        output,
        "| {} | {:.1}% |",
        l.get("upper_bound")?,
        potential * 100.0
    )?;
    writeln!(
        output,
        "| {} | {:.1} KB |",
        l.get("known_recoverable_kb")?,
        summary.gain_bytes as f64 / 1024.0
    )?;
    output.push('\n');

    writeln!(output, "### {}\n", l.get("composition_title")?)?;
    writeln!(
        output,
        "| {} | {} |\n|---|---:|",
        l.get("composition_header")?,
        l.get("bytes_header")?
    )?;
    writeln!(
        output,
        "| {} | {} |",
        l.get("reusable")?,
        human_bytes(summary.composition.reusable_bytes)
    )?;
    writeln!(
        output,
        "| {} | {} |",
        l.get("recoverable")?,
        human_bytes(summary.composition.recoverable_bytes)
    )?;
    writeln!(
        output,
        "| {} | {} |",
        l.get("new_content")?,
        human_bytes(summary.composition.new_content_bytes)
    )?;
    writeln!(output, "\n> {}\n", l.get("composition_note")?)?;

    if current >= target {
        writeln!(
            output,
            "{}\n",
            l.format(
                "above_target",
                &[
                    ("delta", format!("{:.1}", (current - target) * 100.0)),
                    ("sessions", summary.sessions.to_string()),
                    ("pairs", summary.request_pairs.to_string()),
                ],
            )?
        )?;
    } else if potential >= target {
        writeln!(
            output,
            "{}\n",
            l.format(
                "reachable_target",
                &[
                    ("distance", format!("{:.1}", (target - current) * 100.0)),
                    ("sessions", summary.sessions.to_string()),
                    ("pairs", summary.request_pairs.to_string()),
                ],
            )?
        )?;
    } else {
        writeln!(
            output,
            "{}\n",
            l.format(
                "below_target",
                &[
                    ("distance", format!("{:.1}", (target - current) * 100.0)),
                    ("potential", format!("{:.1}", potential * 100.0)),
                    ("remaining", format!("{:.1}", (target - potential) * 100.0)),
                    ("new_content", format!("{:.1}", (1.0 - potential) * 100.0)),
                    ("sessions", summary.sessions.to_string()),
                    ("pairs", summary.request_pairs.to_string()),
                ],
            )?
        )?;
    }

    render_cross_check(output, summary, cache_metrics, locale)
}

#[derive(Default)]
struct SessionHitAggregate {
    prompt_tokens: u64,
    cached_tokens: u64,
    reported: usize,
    estimated: usize,
}

fn render_cross_check(
    output: &mut String,
    summary: &PrefixReuseSummary,
    cache_metrics: &[KvCacheMetric],
    locale: LocaleSnapshot,
) -> Result<()> {
    let l = locale.create(namespace::REPORT_PREFIX_REUSE)?;
    if cache_metrics.is_empty() || summary.by_session.is_empty() {
        return Ok(());
    }
    let mut aggregates: BTreeMap<&str, SessionHitAggregate> = BTreeMap::new();
    for metric in cache_metrics {
        let Some(session_key) = metric.session_key.as_deref() else {
            continue;
        };
        let aggregate = aggregates.entry(session_key).or_default();
        match metric.basis {
            CacheMetricBasis::Reported => {
                aggregate.reported += 1;
                aggregate.prompt_tokens += u64::from(metric.prompt_tokens.unwrap_or(0));
                aggregate.cached_tokens += u64::from(metric.cached_tokens.unwrap_or(0));
            }
            CacheMetricBasis::Estimated => aggregate.estimated += 1,
        }
    }
    let mut rows = Vec::new();
    for session in &summary.by_session {
        let Some(aggregate) = aggregates.get(session.session_key.as_str()) else {
            continue;
        };
        let real_hit_rate = (aggregate.reported > 0 && aggregate.prompt_tokens > 0)
            .then(|| aggregate.cached_tokens as f64 / aggregate.prompt_tokens as f64);
        rows.push((
            session.session_key.as_str(),
            session.ratio,
            real_hit_rate,
            aggregate.reported,
            aggregate.estimated,
        ));
    }
    if rows.is_empty() {
        return Ok(());
    }
    writeln!(output, "### {}\n", l.get("cross_title")?)?;
    writeln!(output, "{}\n", l.get("cross_intro")?)?;
    let estimated_only = rows.len() - rows.iter().filter(|row| row.2.is_some()).count();
    if estimated_only == rows.len() {
        writeln!(
            output,
            "{}\n",
            l.format("estimated_only", &[("sessions", rows.len().to_string())])?
        )?;
        return Ok(());
    }
    rows.retain(|row| row.2.is_some());
    rows.sort_by(|left, right| {
        let left_delta = (left.1 - left.2.unwrap_or(0.0)).abs();
        let right_delta = (right.1 - right.2.unwrap_or(0.0)).abs();
        right_delta
            .partial_cmp(&left_delta)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.cmp(right.0))
    });
    let total = rows.len();
    let truncated = total > MAX_CROSS_CHECK_ROWS;
    rows.truncate(MAX_CROSS_CHECK_ROWS);
    if estimated_only > 0 {
        writeln!(
            output,
            "{}\n",
            l.format(
                "other_estimated",
                &[("sessions", estimated_only.to_string())]
            )?
        )?;
    }
    if truncated {
        writeln!(
            output,
            "{}\n",
            l.format(
                "cross_truncated",
                &[
                    ("shown", MAX_CROSS_CHECK_ROWS.to_string()),
                    ("total", total.to_string()),
                ]
            )?
        )?;
    }
    writeln!(
        output,
        "| {} | {} | {} | {} | {} |",
        l.get("session_header")?,
        l.get("structural_header")?,
        l.get("real_header")?,
        l.get("delta_header")?,
        l.get("coverage_header")?
    )?;
    output.push_str("|---|---:|---:|---:|---|\n");
    for (session_key, structural, real_hit_rate, reported, estimated) in rows {
        let rate = real_hit_rate.expect("filtered to real hit rates");
        writeln!(
            output,
            "| {} | {:.1}% | {:.1}% | {:+.1}pp | {} |",
            code(session_key),
            structural * 100.0,
            rate * 100.0,
            (structural - rate) * 100.0,
            l.format(
                "coverage",
                &[
                    ("reported", reported.to_string()),
                    ("estimated", estimated.to_string()),
                ]
            )?
        )?;
    }
    output.push('\n');
    Ok(())
}
