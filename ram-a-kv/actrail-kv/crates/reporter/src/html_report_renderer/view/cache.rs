//! 本文件把逐请求 KV 命中指标转换为模板消费的表格模型。

use anyhow::Result;

use actrail_kv_artifacts::{AnalysisResult, CacheMetricBasis, KvCacheMetric};

use crate::locale::{namespace, LocaleSnapshot};

pub(crate) struct CacheView {
    pub(crate) title: String,
    pub(crate) intro: String,
    pub(crate) request_count: usize,
    pub(crate) requests_label: String,
    pub(crate) average: String,
    pub(crate) average_label: String,
    pub(crate) reported: usize,
    pub(crate) reported_label: String,
    pub(crate) estimated: usize,
    pub(crate) estimated_label: String,
    pub(crate) session_header: String,
    pub(crate) sequence_header: String,
    pub(crate) request_header: String,
    pub(crate) prompt_header: String,
    pub(crate) cached_header: String,
    pub(crate) miss_header: String,
    pub(crate) output_header: String,
    pub(crate) ttft_header: String,
    pub(crate) total_header: String,
    pub(crate) hit_rate_header: String,
    pub(crate) delta_header: String,
    pub(crate) basis_header: String,
    pub(crate) detail_header: String,
    pub(crate) groups: Vec<CacheGroupView>,
}

pub(crate) struct CacheGroupView {
    pub(crate) label: String,
    pub(crate) rows: Vec<CacheRowView>,
}

pub(crate) struct CacheRowView {
    pub(crate) sequence: usize,
    pub(crate) request_id: String,
    pub(crate) prompt: String,
    pub(crate) cached: String,
    pub(crate) miss: String,
    pub(crate) output: String,
    pub(crate) ttft: String,
    pub(crate) total: String,
    pub(crate) hit_rate: String,
    pub(crate) delta: String,
    pub(crate) basis: String,
    pub(crate) detail: String,
}

pub(crate) fn build(result: &AnalysisResult, locale: LocaleSnapshot) -> Result<CacheView> {
    let l = locale.create(namespace::REPORT_CACHE)?;
    let reported = result
        .cache_metrics
        .iter()
        .filter(|metric| metric.basis == CacheMetricBasis::Reported)
        .count();
    let estimated = result.cache_metrics.len().saturating_sub(reported);
    let average = if result.cache_metrics.is_empty() {
        0.0
    } else {
        result
            .cache_metrics
            .iter()
            .map(|metric| metric.hit_rate)
            .sum::<f64>()
            / result.cache_metrics.len() as f64
    };
    let mut groups: Vec<CacheGroupView> = Vec::new();
    let mut current_session: Option<Option<&str>> = None;
    for metric in &result.cache_metrics {
        let session = metric.session_key.as_deref();
        if current_session != Some(session) {
            current_session = Some(session);
            groups.push(CacheGroupView {
                label: match session {
                    Some(value) => compact_session(value),
                    None => l.get("no_session")?.to_string(),
                },
                rows: Vec::new(),
            });
        }
        groups
            .last_mut()
            .expect("a cache group is created before its first row")
            .rows
            .push(build_row(metric, locale)?);
    }
    Ok(CacheView {
        title: l.get("section_title")?.to_string(),
        intro: l.get("intro")?.to_string(),
        request_count: result.cache_metrics.len(),
        requests_label: l.get("requests")?.to_string(),
        average: format!("{:.1}%", average * 100.0),
        average_label: l.get("average_hit")?.to_string(),
        reported,
        reported_label: l.get("reported")?.to_string(),
        estimated,
        estimated_label: l.get("estimated")?.to_string(),
        session_header: l.get("session")?.to_string(),
        sequence_header: l.get("sequence")?.to_string(),
        request_header: l.get("request")?.to_string(),
        prompt_header: l.get("prompt")?.to_string(),
        cached_header: l.get("cached")?.to_string(),
        miss_header: l.get("miss")?.to_string(),
        output_header: l.get("output")?.to_string(),
        ttft_header: l.get("ttft")?.to_string(),
        total_header: l.get("total")?.to_string(),
        hit_rate_header: l.get("hit_rate")?.to_string(),
        delta_header: l.get("delta")?.to_string(),
        basis_header: l.get("basis")?.to_string(),
        detail_header: l.get("detail")?.to_string(),
        groups,
    })
}

fn build_row(metric: &KvCacheMetric, locale: LocaleSnapshot) -> Result<CacheRowView> {
    let l = locale.create(namespace::REPORT_CACHE)?;
    let tokens = |value: Option<u32>| {
        value
            .map(|tokens| tokens.to_string())
            .unwrap_or_else(|| "—".to_string())
    };
    let latency = |value: Option<u64>| -> Result<String> {
        value
            .map(|ms| l.format("milliseconds", &[("value", ms.to_string())]))
            .unwrap_or_else(|| Ok("—".to_string()))
    };
    let delta = metric
        .hit_rate_delta
        .map(|delta| {
            format!(
                "{}{:.1}pp",
                if delta >= 0.0 { "+" } else { "" },
                delta * 100.0
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
    Ok(CacheRowView {
        sequence: metric.sequence,
        request_id: metric.request_id.chars().take(12).collect(),
        prompt: tokens(metric.prompt_tokens),
        cached: tokens(metric.cached_tokens),
        miss: tokens(metric.miss_tokens),
        output: tokens(metric.output_tokens),
        ttft: latency(metric.ttft_ms)?,
        total: latency(metric.total_ms)?,
        hit_rate: format!("{:.1}%", metric.hit_rate * 100.0),
        delta,
        basis: match metric.basis {
            CacheMetricBasis::Reported => l.get("basis_reported")?.to_string(),
            CacheMetricBasis::Estimated => l.get("basis_estimated")?.to_string(),
        },
        detail,
    })
}

fn compact_session(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 20 {
        return value.to_string();
    }
    let head: String = chars.iter().take(8).collect();
    let tail: String = chars.iter().skip(chars.len().saturating_sub(12)).collect();
    format!("{head}…{tail}")
}
