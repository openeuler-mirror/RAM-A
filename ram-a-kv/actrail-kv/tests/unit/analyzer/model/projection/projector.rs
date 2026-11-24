//! Projector tests verify deterministic order, exact ranges, hierarchy, and explicit limits.

use serde_json::{json, Value};

use super::*;
use crate::model::corpus::{CaptureComparison, CorpusRecord};

fn record(payload: Value) -> CorpusRecord {
    CorpusRecord {
        id: "request".into(),
        captured_at: Some("2026-09-02T08:00:00Z".into()),
        source: None,
        session_key: None,
        comparison: CaptureComparison {
            endpoint_key: "chat".into(),
            agent_key: None,
            model_deployment_key: None,
            kv_namespace: None,
        },
        payload,
        response_usage: None,
        input_line: 1,
    }
}

#[test]
fn ignores_root_key_order_but_preserves_message_and_tool_order() {
    let payload = json!({
        "temperature": 0.9,
        "messages": [{"content":"你好🌍", "role":"system"}, {"role":"user","content":"go"}],
        "tools": [
            {"type":"function", "function":{"name":"zeta", "description":"z"}},
            {"function":{"description":"a", "name":"alpha"}, "type":"function"}
        ],
        "model":"m"
    });
    let projected = RequestProjector::default()
        .project(&record(payload))
        .unwrap();
    let tool_ids: Vec<_> = projected
        .units
        .iter()
        .filter_map(|unit| unit.tool_identity.as_deref())
        .collect();
    assert_eq!(tool_ids, ["zeta", "alpha"]);
    let unicode = projected
        .units
        .iter()
        .find(|unit| unit.content == "你好🌍")
        .unwrap();
    assert_eq!(unicode.source.utf8_bytes, Some(0..10));
    assert!(unicode.content.is_char_boundary(10));
}

#[test]
fn content_arrays_preserve_block_hierarchy_and_exact_text_ranges() {
    let projected = RequestProjector::default()
        .project(&record(json!({
            "model": "m",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "input_text", "text": "你好🌍"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,x"}}
                ]
            }]
        })))
        .unwrap();
    let blocks: Vec<_> = projected
        .units
        .iter()
        .filter(|unit| unit.hierarchy.collection == ContextCollectionKind::ContentBlocks)
        .collect();
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].source.json_path, "$.messages[0].content[0].text");
    assert_eq!(blocks[0].source.utf8_bytes, Some(0..10));
    assert_eq!(blocks[0].hierarchy.content_block_index, Some(0));
    assert_eq!(blocks[1].source.utf8_bytes, None);
    assert_eq!(blocks[1].hierarchy.content_block_index, Some(1));
}

#[test]
fn isolates_models_and_ignores_non_context_fields() {
    let a = RequestProjector::default()
        .project(&record(
            json!({"model":"a", "temperature":0, "messages":[]}),
        ))
        .unwrap();
    let b = RequestProjector::default()
        .project(&record(
            json!({"messages":[], "temperature":1, "model":"b"}),
        ))
        .unwrap();
    assert_ne!(a.domain, b.domain);
    assert_eq!(a.units, b.units);
}

#[test]
fn rejects_at_text_budget_plus_one_without_truncation() {
    let projector = RequestProjector::new(
        ProjectionLimits {
            max_units: 512,
            max_text_unit_bytes: 4,
        },
        3_600,
    );
    let failure = projector
        .project(&record(
            json!({"model":"m", "messages":[{"role":"user", "content":"12345"}]}),
        ))
        .unwrap_err();
    assert!(matches!(
        failure.reason,
        ProjectionSkipReason::TextBudgetExceeded { .. }
    ));
}

#[test]
fn rejects_unit_budget_during_projection() {
    let projector = RequestProjector::new(
        ProjectionLimits {
            max_units: 3,
            max_text_unit_bytes: 1_024,
        },
        3_600,
    );
    let failure = projector
        .project(&record(json!({
            "model": "m",
            "messages": [
                {"role": "user", "content": "one"},
                {"role": "assistant", "content": "two"}
            ]
        })))
        .unwrap_err();
    assert_eq!(
        failure.reason,
        ProjectionSkipReason::UnitBudgetExceeded { limit: 3 }
    );
}
