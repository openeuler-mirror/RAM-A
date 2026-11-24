//! 本文件验证万条同域语料不会被候选预算任意切碎或退化为全量请求对。

use std::io::Cursor;

use actrail_kv_analyzer::run::{analyze_reader, AnalysisOptions};
use serde_json::json;

#[test]
fn ten_thousand_homogeneous_requests_form_one_cohort() {
    const RECORDS: usize = 10_000;
    let line = serde_json::to_string(&json!({
        "captured_at":"2026-01-01T00:00:00Z",
        "comparison":{"endpoint_key":"primary"},
        "payload": {
            "model":"scale-model",
            "messages":[{"role":"system","content":"a stable prompt repeated across a large offline corpus"}]
        }
    }))
    .expect("fixture");
    let mut input = String::with_capacity((line.len() + 1) * RECORDS);
    for _ in 0..RECORDS {
        input.push_str(&line);
        input.push('\n');
    }

    let result = analyze_reader(Cursor::new(input), AnalysisOptions::default()).expect("analysis");
    assert_eq!(result.run.analyzed_records, RECORDS);
    assert_eq!(result.templates.len(), 1);
    assert_eq!(result.templates[0].member_request_ids.len(), RECORDS);
    assert!(result.defects.is_empty());
    assert!(result
        .run
        .limitations
        .iter()
        .all(|limitation| !limitation.contains("candidate medoid budget")));
}
