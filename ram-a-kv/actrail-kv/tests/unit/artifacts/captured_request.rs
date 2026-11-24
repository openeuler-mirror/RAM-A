//! 本文件验证 captured_request 模块的行为。

use super::{CapturedRequest, ComparisonMetadata};
use serde_json::json;

#[test]
fn round_trips_without_losing_payload_order_or_values() {
    let record = CapturedRequest {
        captured_at: "2026-09-02T08:09:10.123456789Z".to_owned(),
        source: Some("instrumentation/agent-a".to_owned()),
        session_key: Some("session-a".to_owned()),
        comparison: comparison(),
        payload: json!({"model": "example", "messages": [{"role": "user", "content": "你好"}]}),
    };

    let encoded = serde_json::to_string(&record).expect("serialize captured request");
    let decoded: CapturedRequest =
        serde_json::from_str(&encoded).expect("deserialize captured request");

    assert_eq!(decoded, record);
    assert_eq!(decoded.session_key.as_deref(), Some("session-a"));
}

#[test]
fn omits_absent_source_but_accepts_it_on_round_trip() {
    let record = CapturedRequest {
        captured_at: "2026-09-02T08:09:10Z".to_owned(),
        source: None,
        session_key: None,
        comparison: comparison(),
        payload: json!({}),
    };

    let encoded = serde_json::to_value(&record).expect("serialize captured request");
    assert!(encoded.get("source").is_none());
    assert_eq!(
        serde_json::from_value::<CapturedRequest>(encoded).expect("deserialize record"),
        record
    );
}

#[test]
fn comparison_endpoint_is_required_but_dimensions_are_optional() {
    let missing_endpoint = json!({
        "captured_at": "2026-09-02T08:09:10Z",
        "comparison": {},
        "payload": {}
    });
    assert!(serde_json::from_value::<CapturedRequest>(missing_endpoint).is_err());

    let minimal = json!({
        "captured_at": "2026-09-02T08:09:10Z",
        "comparison": {"endpoint_key": "primary"},
        "payload": {}
    });
    let decoded: CapturedRequest = serde_json::from_value(minimal).expect("minimal envelope");
    assert_eq!(decoded.comparison.endpoint_key, "primary");
    assert_eq!(decoded.session_key, None);
    assert_eq!(decoded.comparison.agent_key, None);
    assert_eq!(decoded.comparison.model_deployment_key, None);
    assert_eq!(decoded.comparison.kv_namespace, None);
}

fn comparison() -> ComparisonMetadata {
    ComparisonMetadata {
        endpoint_key: "llm-primary".to_owned(),
        agent_key: Some("coding-agent".to_owned()),
        model_deployment_key: Some("deployment-a".to_owned()),
        kv_namespace: None,
    }
}
