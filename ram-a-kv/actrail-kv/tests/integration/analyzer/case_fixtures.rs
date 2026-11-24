//! 用 examples/cases 下真实语料做回归：默认阈值应检出对应缺陷，session 诊断非空。

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

use actrail_kv_analyzer::run::{analyze_reader, AnalysisOptions};
use actrail_kv_artifacts::{AnalysisResult, DefectFactKind};

fn fixture(name: &str) -> AnalysisResult {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/cases")
        .join(format!("{name}.ndjson"));
    analyze_reader(
        BufReader::new(File::open(path).expect("fixture exists")),
        AnalysisOptions {
            max_alignment_cells: 300_000_000,
            max_total_alignment_cells: 8_000_000_000,
            ..AnalysisOptions::default()
        },
    )
    .expect("analysis succeeds")
}

fn has_fact(result: &AnalysisResult, kind: DefectFactKind) -> bool {
    result
        .defects
        .iter()
        .flat_map(|defect| &defect.mismatch.facts)
        .any(|fact| fact.kind == kind)
}

#[test]
fn workspace_fixture_detects_content_variation() {
    let result = fixture("workspace");
    assert!(!result.defects.is_empty(), "result={result:#?}");
    assert!(has_fact(&result, DefectFactKind::ContentVariation));
}

#[test]
fn policy_fixture_detects_fixed_variants() {
    let result = fixture("policy");
    assert!(!result.defects.is_empty(), "result={result:#?}");
    assert!(has_fact(&result, DefectFactKind::FixedVariants));
}

#[test]
fn json_fixture_reports_content_defect() {
    let result = fixture("json");
    assert!(!result.defects.is_empty(), "result={result:#?}");
    assert!(
        has_fact(&result, DefectFactKind::ContentVariation)
            || has_fact(&result, DefectFactKind::StructuredDataEquivalent)
    );
}

#[test]
fn insert_fixture_reports_a_defect() {
    let result = fixture("insert");
    assert!(!result.defects.is_empty(), "result={result:#?}");
}

#[test]
fn natural_fixture_analyzes_without_regression() {
    let result = fixture("natural");
    assert!(result.run.analyzed_records >= 3);
}

#[test]
fn session_fixture_reports_prefix_switch_evidence() {
    let result = fixture("session");
    assert!(!result.session_reports.is_empty(), "result={result:#?}");
    assert!(result
        .session_reports
        .iter()
        .flat_map(|report| &report.events)
        .any(|event| event.lcp_units > 0));
}

#[test]
fn context_refresh_fixture_reports_one_warning_and_clean_session() {
    let result = fixture("session-context-refresh");
    let refresh = result
        .session_reports
        .iter()
        .find(|report| report.session_key == "demo-context-refresh")
        .expect("refresh session");
    let clean = result
        .session_reports
        .iter()
        .find(|report| report.session_key == "demo-append-only")
        .expect("append-only session");

    assert_eq!(refresh.context_refresh_count, 1);
    assert_eq!(refresh.total_abandoned_prefix_bytes, 4);
    assert_eq!(clean.context_refresh_count, 0);
}
