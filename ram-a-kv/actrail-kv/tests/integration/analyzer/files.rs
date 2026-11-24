//! 本文件验证分析文件边界和报告输入筛选行为。

use std::{fs, io::Cursor, num::NonZeroUsize};

use actrail_kv_analyzer::run::{
    analyze_file, analyze_file_with_selection, analyze_reader, AnalysisOptions, CaptureTimeRange,
    ReportSelection,
};
use serde_json::{json, Value};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

#[test]
fn refuses_same_input_and_output_without_modifying_corpus() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("requests.ndjson");
    let original = br#"{"payload":{"model":"m","messages":[]}}
"#;
    fs::write(&path, original).expect("fixture");

    assert!(analyze_file(&path, &path, AnalysisOptions::default()).is_err());
    assert_eq!(fs::read(&path).expect("preserved corpus"), original);
}

#[cfg(unix)]
#[test]
fn refuses_output_that_is_a_hard_link_to_input() {
    let directory = tempfile::tempdir().expect("tempdir");
    let input = directory.path().join("requests.ndjson");
    let output = directory.path().join("alias.json");
    let original = br#"{"payload":{"model":"m","messages":[]}}
"#;
    fs::write(&input, original).expect("fixture");
    fs::hard_link(&input, &output).expect("hard link");

    assert!(analyze_file(&input, &output, AnalysisOptions::default()).is_err());
    assert_eq!(fs::read(&input).expect("preserved corpus"), original);
}

#[test]
fn rejects_a_corpus_with_no_analyzable_requests() {
    let directory = tempfile::tempdir().expect("tempdir");
    let input = directory.path().join("requests.ndjson");
    let output = directory.path().join("analysis.json");
    fs::write(&input, b"not-json\n").expect("fixture");

    assert!(analyze_file(&input, &output, AnalysisOptions::default()).is_err());
    assert!(!output.exists());
}

#[test]
fn report_selects_the_latest_matching_captures_before_the_analysis_budget() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("requests.ndjson");
    let output = directory.path().join("analysis.json");
    let records = [
        capture("a", "2026-11-22T23:00:00+01:00", "latest"),
        capture("b", "2026-11-22T12:00:00Z", "earlier tie"),
        capture("a", "2026-11-21T12:00:00Z", "gap"),
        capture("b", "2026-11-20T00:00:00Z", "first window"),
        capture("c", "2026-11-22T23:00:00Z", "other session"),
        capture("a", "2026-11-23T00:00:00Z", "exclusive end"),
        capture("b", "2026-11-22T13:00:00+01:00", "later tie"),
        capture("a", "invalid", "invalid timestamp"),
    ];
    fs::write(&input, ndjson(&records)).unwrap();
    let ranges = vec![
        range("2026-11-20T00:00:00Z", "2026-11-21T00:00:00Z"),
        range("2026-11-22T00:00:00Z", "2026-11-23T00:00:00Z"),
        range("2026-11-22T06:00:00Z", "2026-11-23T00:00:00Z"),
    ];
    let sessions = vec!["a".into(), "b".into()];
    let selection = ReportSelection::new(sessions.clone(), ranges.clone(), NonZeroUsize::new(2));
    let options = AnalysisOptions {
        max_records: 2,
        ..AnalysisOptions::default()
    };
    let result = analyze_file_with_selection(&input, &output, options, &selection).unwrap();
    assert_eq!(result.run.input_records, 2);
    let expected = analyze_reader(
        Cursor::new(ndjson(&[records[0].clone(), records[6].clone()])),
        AnalysisOptions::default(),
    )
    .unwrap();
    let ids = |result: &actrail_kv_artifacts::AnalysisResult| {
        result
            .cache_metrics
            .iter()
            .map(|metric| metric.request_id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&result), ids(&expected));
    assert_eq!(ids(&result).len(), 2);

    let selection = ReportSelection::new(sessions, ranges, None);
    let result =
        analyze_file_with_selection(&input, &output, AnalysisOptions::default(), &selection)
            .unwrap();
    assert_eq!(result.run.input_records, 4);
}

fn capture(session: &str, at: &str, message: &str) -> Value {
    json!({"captured_at": at, "session_key": session, "comparison": {"endpoint_key": "test"},
        "payload": {"model":"m", "messages":[{"role":"user","content":message}]},
        "legacy_extra_field": true})
}

fn ndjson(records: &[Value]) -> String {
    records.iter().map(|record| format!("{record}\n")).collect()
}

fn range(from: &str, until: &str) -> CaptureTimeRange {
    let parse = |value| OffsetDateTime::parse(value, &Rfc3339).unwrap();
    CaptureTimeRange::new(Some(parse(from)), Some(parse(until))).unwrap()
}
