//! 本文件验证 discovery/candidate/signature 模块的行为。

use super::*;
use crate::model::projection::{
    ComparisonDomain, ContextCollectionKind, HierarchyLocation, SourceLocation,
};

fn sequence(model: &str, units: Vec<CacheUnit>) -> CacheSequence {
    CacheSequence {
        request_id: model.into(),
        domain: ComparisonDomain {
            time_window_key: 0,
            endpoint_key: "chat".into(),
            dialect: "d".into(),
            model: model.into(),
            model_deployment_key: None,
            context_schema_key: "chat/v1".into(),
            agent_key: None,
            kv_namespace: None,
            adapter_revision: "1".into(),
        },
        units,
        projection_reliability_millis: 1000,
    }
}

fn unit(kind: CacheUnitKind, key: &str, content: String, tool: Option<&str>) -> CacheUnit {
    CacheUnit {
        kind,
        alignment_key: key.into(),
        source: SourceLocation {
            json_path: "$".into(),
            utf8_bytes: Some(0..content.len()),
            message_index: None,
            role: None,
        },
        content,
        tool_identity: tool.map(str::to_owned),
        hierarchy: HierarchyLocation {
            collection: ContextCollectionKind::Messages,
            parent_json_path: "$.messages".into(),
            element_index: None,
            content_block_index: None,
        },
    }
}

#[test]
fn metrics_are_symmetric_and_cross_domain_isolated() {
    let a = sequence(
        "m",
        vec![unit(CacheUnitKind::VisibleText, "x", "alpha".into(), None)],
    );
    let b = sequence(
        "m",
        vec![unit(CacheUnitKind::VisibleText, "x", "alpHa".into(), None)],
    );
    assert_eq!(pair_metrics(&a, &b), pair_metrics(&b, &a));
    let other = sequence("other", b.units.clone());
    assert_eq!(pair_metrics(&a, &other).compatibility, 0.0);
}

#[test]
fn huge_changed_text_and_tool_schema_are_not_hidden_by_small_units() {
    let stable = |value: &str| unit(CacheUnitKind::Role, "role", value.into(), None);
    let a = sequence(
        "m",
        vec![
            stable("system"),
            stable("same"),
            unit(
                CacheUnitKind::ToolDefinition,
                "tool",
                "a".repeat(100_000),
                Some("lookup"),
            ),
        ],
    );
    let b = sequence(
        "m",
        vec![
            stable("system"),
            stable("same"),
            unit(
                CacheUnitKind::ToolDefinition,
                "tool",
                "z".repeat(100_000),
                Some("lookup"),
            ),
        ],
    );
    let metrics = pair_metrics(&a, &b);
    assert!(metrics.dynamic_coverage_ratio > 0.95, "{metrics:?}");
    assert!(metrics.compatibility < 0.68, "{metrics:?}");
}
