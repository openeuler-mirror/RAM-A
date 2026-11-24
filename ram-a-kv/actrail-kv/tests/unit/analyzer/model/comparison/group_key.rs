//! 本文件验证 model/comparison/group_key 模块的行为。

use serde_json::json;

use super::*;
use crate::model::corpus::{CaptureComparison, CorpusRecord};

fn record(captured_at: &str) -> CorpusRecord {
    CorpusRecord {
        id: captured_at.into(),
        captured_at: Some(captured_at.into()),
        source: None,
        session_key: None,
        comparison: CaptureComparison {
            endpoint_key: "chat".into(),
            agent_key: Some("agent-a".into()),
            model_deployment_key: Some("deployment-a".into()),
            kv_namespace: Some("tenant-a".into()),
        },
        payload: json!({"model": "model-a", "messages": []}),
        response_usage: None,
        input_line: 1,
    }
}

fn key(record: &CorpusRecord) -> ComparisonGroupKey {
    ComparisonGroupKey::from_record(record, 3_600, "chat", "1", "chat/v1").unwrap()
}

#[test]
fn fixed_windows_are_utc_epoch_aligned_and_end_is_exclusive() {
    let before = key(&record("2026-09-02T08:59:59.999Z"));
    let boundary = key(&record("2026-09-02T09:00:00Z"));
    assert_eq!(boundary.time_window_key - before.time_window_key, 3_600);
}

#[test]
fn every_explicit_cache_scope_dimension_is_part_of_identity() {
    let baseline_record = record("2026-09-02T08:00:00Z");
    let baseline = key(&baseline_record);
    let mut variants = Vec::new();
    let mut endpoint = baseline_record.clone();
    endpoint.comparison.endpoint_key = "responses".into();
    variants.push(key(&endpoint));
    let mut deployment = baseline_record.clone();
    deployment.comparison.model_deployment_key = None;
    variants.push(key(&deployment));
    let mut agent = baseline_record.clone();
    agent.comparison.agent_key = None;
    variants.push(key(&agent));
    let mut namespace = baseline_record.clone();
    namespace.comparison.kv_namespace = None;
    variants.push(key(&namespace));
    let mut model = baseline_record.clone();
    model.payload["model"] = json!("model-b");
    variants.push(key(&model));
    assert!(variants.iter().all(|variant| variant != &baseline));
}

#[test]
fn invalid_timestamp_and_zero_window_are_rejected() {
    let invalid = record("not-a-time");
    assert!(matches!(
        ComparisonGroupKey::from_record(&invalid, 3_600, "chat", "1", "chat/v1"),
        Err(ComparisonGroupError::InvalidCapturedAt(_))
    ));
    assert_eq!(
        ComparisonGroupKey::from_record(&record("2026-09-02T08:00:00Z"), 0, "chat", "1", "chat/v1"),
        Err(ComparisonGroupError::InvalidWindowSeconds)
    );
}
