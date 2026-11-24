//! 本文件把结构复用率、字节构成和真实命中交叉验证转换为展示模型。

use std::collections::BTreeMap;

use anyhow::Result;

use actrail_kv_artifacts::{CacheMetricBasis, KvCacheMetric, PrefixReuseSummary};

use super::super::format::human_bytes;
use crate::locale::{namespace, LocaleSnapshot};

const PREFIX_REUSE_TARGET: f64 = 0.9;
const MAX_CROSS_CHECK_ROWS: usize = 8;

pub(crate) struct PrefixReuseView {
    pub(crate) title: String,
    pub(crate) scope: String,
    pub(crate) current_width: String,
    pub(crate) gain_left: String,
    pub(crate) gain_width: String,
    pub(crate) new_left: String,
    pub(crate) new_width: String,
    pub(crate) target_left: String,
    pub(crate) current_label: String,
    pub(crate) current_percent: String,
    pub(crate) upper_label: String,
    pub(crate) upper_percent: String,
    pub(crate) known_recoverable_label: String,
    pub(crate) known_recoverable_kb: String,
    pub(crate) target_label: String,
    pub(crate) target_percent: String,
    pub(crate) composition_title: String,
    pub(crate) reusable_label: String,
    pub(crate) reusable_value: String,
    pub(crate) recoverable_label: String,
    pub(crate) recoverable_value: String,
    pub(crate) new_content_label: String,
    pub(crate) new_content_value: String,
    pub(crate) composition_note: String,
    pub(crate) status_note: String,
    pub(crate) cross_check: Option<CrossCheckView>,
}

pub(crate) struct CrossCheckView {
    pub(crate) title: String,
    pub(crate) intro: String,
    pub(crate) estimated_only: Option<String>,
    pub(crate) other_estimated: Option<String>,
    pub(crate) truncated: Option<String>,
    pub(crate) session_header: String,
    pub(crate) structural_header: String,
    pub(crate) real_header: String,
    pub(crate) delta_header: String,
    pub(crate) coverage_header: String,
    pub(crate) rows: Vec<CrossCheckRow>,
}

pub(crate) struct CrossCheckRow {
    pub(crate) session: String,
    pub(crate) structural: String,
    pub(crate) real: String,
    pub(crate) delta: String,
    pub(crate) coverage: String,
}

#[derive(Default)]
struct SessionHitAggregate {
    prompt_tokens: u64,
    cached_tokens: u64,
    reported: usize,
    estimated: usize,
}

pub(crate) fn build(
    summary: &PrefixReuseSummary,
    cache_metrics: &[KvCacheMetric],
    locale: LocaleSnapshot,
) -> Result<PrefixReuseView> {
    let l = locale.create(namespace::REPORT_PREFIX_REUSE)?;
    let current = summary.ratio.clamp(0.0, 1.0);
    let potential = summary.potential_ratio.clamp(current, 1.0);
    let target = PREFIX_REUSE_TARGET.clamp(0.0, 1.0);
    let status_note = if current >= target {
        l.format(
            "above_target",
            &[
                ("delta", format!("{:.1}", (current - target) * 100.0)),
                ("sessions", summary.sessions.to_string()),
                ("pairs", summary.request_pairs.to_string()),
            ],
        )?
    } else if potential >= target {
        l.format(
            "reachable_target",
            &[
                ("distance", format!("{:.1}", (target - current) * 100.0)),
                ("sessions", summary.sessions.to_string()),
                ("pairs", summary.request_pairs.to_string()),
            ],
        )?
    } else {
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
    };
    Ok(PrefixReuseView {
        title: l.get("section_title")?.to_string(),
        scope: l.get("scope")?.to_string(),
        current_width: format!("{:.2}", current * 100.0),
        gain_left: format!("{:.2}", current * 100.0),
        gain_width: format!("{:.2}", (potential - current) * 100.0),
        new_left: format!("{:.2}", potential * 100.0),
        new_width: format!("{:.2}", (1.0 - potential) * 100.0),
        target_left: format!("{:.2}", target * 100.0),
        current_label: l.get("current")?.to_string(),
        current_percent: format!("{:.1}", current * 100.0),
        upper_label: l.get("upper_bound")?.to_string(),
        upper_percent: format!("{:.1}", potential * 100.0),
        known_recoverable_label: l.get("known_recoverable_kb")?.to_string(),
        known_recoverable_kb: format!("{:.1}", summary.gain_bytes as f64 / 1024.0),
        target_label: l.get("target")?.to_string(),
        target_percent: format!("{:.0}", target * 100.0),
        composition_title: l.get("composition_title")?.to_string(),
        reusable_label: l.get("reusable")?.to_string(),
        reusable_value: human_bytes(summary.composition.reusable_bytes),
        recoverable_label: l.get("recoverable")?.to_string(),
        recoverable_value: human_bytes(summary.composition.recoverable_bytes),
        new_content_label: l.get("new_content")?.to_string(),
        new_content_value: human_bytes(summary.composition.new_content_bytes),
        composition_note: l.get("composition_note")?.to_string(),
        status_note,
        cross_check: build_cross_check(summary, cache_metrics, locale)?,
    })
}

fn build_cross_check(
    summary: &PrefixReuseSummary,
    cache_metrics: &[KvCacheMetric],
    locale: LocaleSnapshot,
) -> Result<Option<CrossCheckView>> {
    let l = locale.create(namespace::REPORT_PREFIX_REUSE)?;
    if cache_metrics.is_empty() || summary.by_session.is_empty() {
        return Ok(None);
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
    let mut rows: Vec<_> = summary
        .by_session
        .iter()
        .filter_map(|session| {
            let aggregate = aggregates.get(session.session_key.as_str())?;
            let real_hit_rate = (aggregate.reported > 0 && aggregate.prompt_tokens > 0)
                .then(|| aggregate.cached_tokens as f64 / aggregate.prompt_tokens as f64);
            Some((
                session.session_key.as_str(),
                session.ratio,
                real_hit_rate,
                aggregate.reported,
                aggregate.estimated,
            ))
        })
        .collect();
    if rows.is_empty() {
        return Ok(None);
    }
    let estimated_only = rows.len() - rows.iter().filter(|row| row.2.is_some()).count();
    let estimated_only_message = (estimated_only == rows.len())
        .then(|| l.format("estimated_only", &[("sessions", rows.len().to_string())]))
        .transpose()?;
    if estimated_only == rows.len() {
        return Ok(Some(CrossCheckView {
            title: l.get("cross_title")?.to_string(),
            intro: l.get("cross_intro")?.to_string(),
            estimated_only: estimated_only_message,
            other_estimated: None,
            truncated: None,
            session_header: l.get("session_header")?.to_string(),
            structural_header: l.get("structural_header")?.to_string(),
            real_header: l.get("real_header")?.to_string(),
            delta_header: l.get("delta_header")?.to_string(),
            coverage_header: l.get("coverage_header")?.to_string(),
            rows: Vec::new(),
        }));
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
    Ok(Some(CrossCheckView {
        title: l.get("cross_title")?.to_string(),
        intro: l.get("cross_intro")?.to_string(),
        estimated_only: None,
        other_estimated: (estimated_only > 0)
            .then(|| {
                l.format(
                    "other_estimated",
                    &[("sessions", estimated_only.to_string())],
                )
            })
            .transpose()?,
        truncated: truncated
            .then(|| {
                l.format(
                    "cross_truncated",
                    &[
                        ("shown", MAX_CROSS_CHECK_ROWS.to_string()),
                        ("total", total.to_string()),
                    ],
                )
            })
            .transpose()?,
        session_header: l.get("session_header")?.to_string(),
        structural_header: l.get("structural_header")?.to_string(),
        real_header: l.get("real_header")?.to_string(),
        delta_header: l.get("delta_header")?.to_string(),
        coverage_header: l.get("coverage_header")?.to_string(),
        rows: rows
            .into_iter()
            .map(
                |(session_key, structural, real_hit_rate, reported, estimated)| {
                    let rate = real_hit_rate.unwrap_or(0.0);
                    Ok(CrossCheckRow {
                        session: session_key.to_string(),
                        structural: format!("{:.1}%", structural * 100.0),
                        real: format!("{:.1}%", rate * 100.0),
                        delta: format!("{:+.1}pp", (structural - rate) * 100.0),
                        coverage: l.format(
                            "coverage",
                            &[
                                ("reported", reported.to_string()),
                                ("estimated", estimated.to_string()),
                            ],
                        )?,
                    })
                },
            )
            .collect::<Result<Vec<_>>>()?,
    }))
}
