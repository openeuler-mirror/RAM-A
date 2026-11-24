//! 本文件验证 cache_metrics 模块的行为。

use super::*;
use crate::model::corpus::{CaptureComparison, ResponseUsage};
use serde_json::json;

fn record(
    id: &str,
    session: Option<&str>,
    messages: serde_json::Value,
    usage: Option<(u32, u32, u32)>,
) -> CorpusRecord {
    CorpusRecord {
        id: id.to_string(),
        captured_at: Some(format!("2026-01-01T00:00:0{id}Z")),
        source: None,
        session_key: session.map(str::to_owned),
        comparison: CaptureComparison {
            endpoint_key: "primary".to_string(),
            agent_key: None,
            model_deployment_key: None,
            kv_namespace: None,
        },
        payload: json!({ "model": "m", "messages": messages }),
        response_usage: usage.map(|(prompt, cached, completion)| ResponseUsage {
            prompt_tokens: prompt,
            cached_tokens: cached,
            completion_tokens: completion,
            ttft_ms: None,
            total_ms: None,
        }),
        input_line: id.parse().unwrap_or(0),
    }
}

#[test]
fn reported_usage_produces_hit_rate_and_delta() {
    let records = vec![
        record(
            "1",
            Some("s"),
            json!([{"role":"user","content":"a"}]),
            Some((1000, 200, 10)),
        ),
        record(
            "2",
            Some("s"),
            json!([{"role":"user","content":"a"},{"role":"user","content":"b"}]),
            Some((1200, 900, 20)),
        ),
    ];
    let metrics = compute_cache_metrics(&records);
    assert_eq!(metrics.len(), 2);
    assert_eq!(metrics[0].basis, CacheMetricBasis::Reported);
    assert!((metrics[0].hit_rate - 0.2).abs() < 1e-9);
    assert!(metrics[0].hit_rate_delta.is_none());
    assert_eq!(metrics[1].cached_tokens, Some(900));
    assert_eq!(metrics[1].miss_tokens, Some(300));
    let delta = metrics[1].hit_rate_delta.expect("delta");
    assert!((delta - 0.55).abs() < 1e-9, "delta={delta}");
}

#[test]
fn latency_is_passed_through_and_never_filled_with_zero() {
    let mut measured = record(
        "1",
        Some("s"),
        json!([{"role":"user","content":"a"}]),
        Some((100, 50, 5)),
    );
    let usage = measured.response_usage.as_mut().expect("usage");
    usage.ttft_ms = Some(120);
    usage.total_ms = Some(900);
    let estimated = record(
        "2",
        Some("s"),
        json!([{"role":"user","content":"a"},{"role":"user","content":"b"}]),
        None,
    );

    let metrics = compute_cache_metrics(&[measured, estimated]);
    assert_eq!(metrics[0].ttft_ms, Some(120));
    assert_eq!(metrics[0].total_ms, Some(900));
    assert_eq!(metrics[0].basis, CacheMetricBasis::Reported);
    assert_eq!(metrics[1].ttft_ms, None);
    assert_eq!(metrics[1].total_ms, None);
    assert_eq!(metrics[1].basis, CacheMetricBasis::Estimated);
}

#[test]
fn missing_usage_falls_back_to_context_prefix_estimate() {
    let records = vec![
        record(
            "1",
            Some("s"),
            json!([{"role":"user","content":"same-prefix"}]),
            None,
        ),
        record(
            "2",
            Some("s"),
            json!([{"role":"user","content":"same-prefix"},{"role":"user","content":"tail"}]),
            None,
        ),
    ];
    let metrics = compute_cache_metrics(&records);
    assert_eq!(metrics[0].basis, CacheMetricBasis::Estimated);
    assert!(metrics[0].prompt_tokens.is_none());
    assert!(metrics[0].estimated_lcp_bytes.is_none());
    assert!(metrics[1].estimated_lcp_bytes.unwrap_or(0) > 0);
    assert!(metrics[1].hit_rate > 0.0);
    assert!(metrics[1].hit_rate_delta.is_some());
}
