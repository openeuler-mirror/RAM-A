//! 本文件验证 analysis_result 模块的行为。

use super::*;

#[test]
fn unified_defect_round_trips_with_snake_case_enums() {
    let result = fixture();
    let encoded = serde_json::to_string(&result).expect("serialize analysis result");
    assert!(encoded.contains("\"pattern\":\"mixed\""));
    assert!(encoded.contains("\"kind\":\"structured_data_equivalent\""));
    assert_eq!(
        serde_json::from_str::<AnalysisResult>(&encoded).expect("deserialize analysis result"),
        result
    );
}

#[test]
fn missing_variant_can_have_no_source_or_excerpt() {
    let mut result = fixture();
    let evidence = &mut result.defects[0].mismatch.variants[1].representative;
    evidence.sources.clear();
    evidence.excerpt = None;
    evidence.utf8_bytes = 0;
    let encoded = serde_json::to_string(&result).expect("serialize analysis result");
    let decoded: AnalysisResult = serde_json::from_str(&encoded).expect("deserialize result");
    assert!(decoded.defects[0].mismatch.variants[1]
        .representative
        .sources
        .is_empty());
}

#[test]
fn all_mismatch_patterns_have_stable_snake_case_names() {
    for (pattern, expected) in [
        (MismatchPattern::ValueMismatch, "\"value_mismatch\""),
        (MismatchPattern::InsertionDeletion, "\"insertion_deletion\""),
        (MismatchPattern::Reorder, "\"reorder\""),
        (MismatchPattern::Mixed, "\"mixed\""),
    ] {
        assert_eq!(
            serde_json::to_string(&pattern).expect("serialize"),
            expected
        );
    }
}

fn fixture() -> AnalysisResult {
    let group = ComparisonGroup {
        time_window_key: "2026-09-02T08:00Z/1h".into(),
        endpoint_key: "primary".into(),
        model: "model-a".into(),
        context_schema_key: "openai-chat/v1".into(),
        agent_key: Some("coding".into()),
        model_deployment_key: Some("deployment-a".into()),
        kv_namespace: None,
    };
    let source = SourceLocation {
        json_path: "$.messages[0].content".into(),
        logical_scope: vec!["messages".into(), "system".into()],
        unit_index: Some(1),
        byte_start: Some(4),
        byte_end: Some(8),
        role: Some("system".into()),
    };
    let score = ScoreBreakdown {
        blocked_stable_bytes: 128,
        affected_count: 2,
        confidence: 0.75,
        score: 192.0,
    };
    AnalysisResult {
        run: AnalysisRunSummary {
            schema_version: ANALYSIS_SCHEMA_VERSION.into(),
            input_records: 4,
            analyzed_records: 4,
            skipped_records: vec![],
            options: options(),
            limitations: vec!["structural proxy only".into()],
        },
        templates: vec![RequestTemplate {
            id: "template-a".into(),
            comparison_group: group.clone(),
            member_request_ids: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            medoid_request_id: "a".into(),
            cohesion: 0.9,
            stable_spans: vec![],
            slots: vec![],
            prefix_view: None,
        }],
        defects: vec![ContextDefect {
            id: "defect-a".into(),
            comparison_group: group,
            template_id: "template-a".into(),
            mismatch: MismatchRegion {
                pattern: MismatchPattern::Mixed,
                variants: vec![
                    variant("variant-a", &["a", "b"], "a", Some("north"), &source),
                    variant("variant-b", &["c", "d"], "c", Some("south"), &source),
                ],
                facts: vec![DefectFact {
                    kind: DefectFactKind::StructuredDataEquivalent,
                    detail: Some("JSON objects are strictly equivalent".into()),
                }],
            },
            recovered_stable: RecoveredStable {
                sources: vec![source],
                utf8_bytes: 128,
                support_count: 4,
                excerpt: "stable P2".into(),
            },
            actual_prefix_bytes: 16,
            potential_prefix_bytes: 144,
            blocked_stable_bytes: 128,
            comparable_count: 4,
            affected_count: 2,
            confidence: 0.75,
            score,
            insights: vec![OptimizationInsight {
                summary: "统一 X 的生成方式".into(),
                detail: Some("在语义允许时将稳定 P2 前移".into()),
            }],
        }],
        top_k: vec!["defect-a".into()],
        session_reports: vec![],
        cache_metrics: vec![],
        prefix_reuse: None,
    }
}

fn variant(
    fingerprint: &str,
    members: &[&str],
    representative: &str,
    excerpt: Option<&str>,
    source: &SourceLocation,
) -> MismatchVariant {
    MismatchVariant {
        fingerprint: fingerprint.into(),
        member_request_ids: members.iter().map(|value| (*value).into()).collect(),
        representative: VariantEvidence {
            request_id: representative.into(),
            sources: vec![source.clone()],
            utf8_bytes: excerpt.map_or(0, str::len),
            excerpt: excerpt.map(str::to_owned),
        },
    }
}

fn options() -> analysis_contract::AnalyzeConfig {
    analysis_contract::AnalyzeConfig {
        top_k: 20,
        ..Default::default()
    }
}
