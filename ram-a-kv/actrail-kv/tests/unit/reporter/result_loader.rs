//! 本文件验证 result_loader 模块的行为。

use std::{fs, path::Path};

use actrail_kv_artifacts::*;

use super::load_result;

#[test]
fn accepts_valid_unified_result() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("analysis.json");
    write(&path, &fixture());
    assert!(load_result(&path).is_ok());
}

#[test]
fn rejects_wrong_schema_and_broken_top_k() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("analysis.json");
    let mut result = fixture();
    result.run.schema_version = "legacy".into();
    write(&path, &result);
    assert!(load_result(&path).is_err());

    let mut result = fixture();
    result.top_k = vec!["unknown".into()];
    write(&path, &result);
    assert!(load_result(&path).is_err());
}

#[test]
fn rejects_overlapping_variant_members_and_inconsistent_score() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("analysis.json");
    let mut result = fixture();
    result.defects[0].mismatch.variants[1]
        .member_request_ids
        .push("a".into());
    write(&path, &result);
    assert!(load_result(&path).is_err());

    let mut result = fixture();
    result.defects[0].score.score += 1.0;
    write(&path, &result);
    assert!(load_result(&path).is_err());
}

#[test]
fn accepts_machine_precision_score_rounding() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("analysis.json");
    let mut result = fixture();
    let defect = &mut result.defects[0];
    let expected =
        defect.blocked_stable_bytes as f64 * defect.affected_count as f64 * defect.confidence;
    defect.score.score = expected + f64::EPSILON * expected * 4.0;
    write(&path, &result);
    assert!(load_result(&path).is_ok());
}

#[test]
fn rejects_cyclic_and_dangling_prefix_views() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("analysis.json");

    let mut self_cycle = fixture();
    self_cycle.templates[0].prefix_view = Some(prefix_view("seq-0", Some("seq-0")));
    write(&path, &self_cycle);
    let error = load_result(&path).expect_err("self cycle must fail");
    assert!(error.to_string().contains("cycle"));

    let mut multi_cycle = fixture();
    multi_cycle.templates[0].prefix_view = Some(TemplatePrefixView {
        node_count: 2,
        truncated: false,
        nodes: vec![
            prefix_node("seq-0", Some("seq-1")),
            prefix_node("seq-1", Some("seq-0")),
        ],
    });
    write(&path, &multi_cycle);
    let error = load_result(&path).expect_err("multi-node cycle must fail");
    assert!(error.to_string().contains("cycle"));

    let mut dangling = fixture();
    dangling.templates[0].prefix_view = Some(prefix_view("seq-0", Some("missing")));
    write(&path, &dangling);
    let error = load_result(&path).expect_err("dangling child must fail");
    assert!(error.to_string().contains("unknown node"));
}

fn prefix_view(id: &str, sequence_child: Option<&str>) -> TemplatePrefixView {
    TemplatePrefixView {
        node_count: 1,
        truncated: false,
        nodes: vec![prefix_node(id, sequence_child)],
    }
}

fn prefix_node(id: &str, sequence_child: Option<&str>) -> PrefixNode {
    PrefixNode {
        id: id.into(),
        kind: PrefixNodeKind::Stable,
        source: SourceLocation {
            json_path: "$.messages[0].content".into(),
            logical_scope: vec![],
            unit_index: Some(0),
            byte_start: Some(0),
            byte_end: Some(0),
            role: None,
        },
        utf8_bytes: 0,
        support_count: 4,
        support_ratio: 1.0,
        excerpt: String::new(),
        member_request_ids: vec![],
        content_sample: None,
        sequence_child: sequence_child.map(str::to_owned),
        internal_children: vec![],
        variants: None,
        defect_ids: vec![],
    }
}

fn write(path: &Path, result: &AnalysisResult) {
    fs::write(path, serde_json::to_vec(result).expect("serialize")).expect("write fixture");
}

pub(crate) fn fixture() -> AnalysisResult {
    let group = ComparisonGroup {
        time_window_key: "window".into(),
        endpoint_key: "endpoint".into(),
        model: "model".into(),
        context_schema_key: "schema".into(),
        agent_key: None,
        model_deployment_key: None,
        kv_namespace: None,
    };
    let source = SourceLocation {
        json_path: "$.messages[0].content".into(),
        logical_scope: vec!["messages".into()],
        unit_index: Some(0),
        byte_start: Some(0),
        byte_end: Some(8),
        role: Some("system".into()),
    };
    let score = ScoreBreakdown {
        blocked_stable_bytes: 100,
        affected_count: 2,
        confidence: 0.9,
        score: 180.0,
    };
    AnalysisResult {
        run: AnalysisRunSummary {
            schema_version: ANALYSIS_SCHEMA_VERSION.into(),
            input_records: 4,
            analyzed_records: 4,
            skipped_records: vec![],
            options: options(),
            limitations: vec![],
        },
        templates: vec![RequestTemplate {
            id: "template".into(),
            comparison_group: group.clone(),
            member_request_ids: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            medoid_request_id: "a".into(),
            cohesion: 0.9,
            stable_spans: vec![],
            slots: vec![],
            prefix_view: None,
        }],
        defects: vec![ContextDefect {
            id: "defect".into(),
            comparison_group: group,
            template_id: "template".into(),
            mismatch: MismatchRegion {
                pattern: MismatchPattern::ValueMismatch,
                variants: vec![
                    variant(
                        "one",
                        &["a", "b"],
                        "a",
                        "<script>alert(1)</script>",
                        &source,
                    ),
                    variant("two", &["c", "d"], "c", "south", &source),
                ],
                facts: vec![DefectFact {
                    kind: DefectFactKind::ContentVariation,
                    detail: Some("<img src=x onerror=alert(1)>".into()),
                }],
            },
            recovered_stable: RecoveredStable {
                sources: vec![source],
                utf8_bytes: 100,
                support_count: 4,
                excerpt: "<script>stable()</script>".into(),
            },
            actual_prefix_bytes: 10,
            potential_prefix_bytes: 110,
            blocked_stable_bytes: 100,
            comparable_count: 4,
            affected_count: 2,
            confidence: 0.9,
            score,
            insights: vec![OptimizationInsight {
                summary: "<b>统一生成方式</b>".into(),
                detail: Some("move & review".into()),
            }],
        }],
        top_k: vec!["defect".into()],
        session_reports: vec![],
        cache_metrics: vec![],
        prefix_reuse: None,
    }
}

fn variant(
    fingerprint: &str,
    members: &[&str],
    representative: &str,
    excerpt: &str,
    source: &SourceLocation,
) -> MismatchVariant {
    MismatchVariant {
        fingerprint: fingerprint.into(),
        member_request_ids: members.iter().map(|member| (*member).into()).collect(),
        representative: VariantEvidence {
            request_id: representative.into(),
            sources: vec![source.clone()],
            utf8_bytes: excerpt.len(),
            excerpt: Some(excerpt.into()),
        },
    }
}

fn options() -> analysis_contract::AnalyzeConfig {
    analysis_contract::AnalyzeConfig {
        top_k: 1,
        ..Default::default()
    }
}
