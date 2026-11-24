//! 本文件以真实 captured NDJSON payload 验收八类通用诊断和关键零误报边界。

use std::io::Cursor;

use actrail_kv_analyzer::run::{analyze_reader, AnalysisOptions};
use actrail_kv_artifacts::{AnalysisResult, DefectFactKind, MismatchPattern, SessionEventType};
use serde_json::{json, Value};

fn stable(label: &str) -> String {
    format!("{label}:{}", "稳定规则内容。".repeat(32))
}

fn analyze(payloads: Vec<Value>) -> AnalysisResult {
    analyze_with_session(payloads, None)
}

fn analyze_with_session(payloads: Vec<Value>, session_key: Option<&str>) -> AnalysisResult {
    analyze_with_options(payloads, session_key, AnalysisOptions::default())
}

fn analyze_with_options(
    payloads: Vec<Value>,
    session_key: Option<&str>,
    options: AnalysisOptions,
) -> AnalysisResult {
    let mut input = String::new();
    for payload in payloads {
        let mut envelope = json!({
            "captured_at":"2026-01-01T00:00:00Z",
            "comparison":{"endpoint_key":"primary"},
            "payload":payload
        });
        if let Some(key) = session_key {
            envelope["session_key"] = json!(key);
        }
        input.push_str(&serde_json::to_string(&envelope).expect("fixture JSON"));
        input.push('\n');
    }
    analyze_reader(Cursor::new(input), options).expect("analysis succeeds")
}

fn analyze_envelopes(envelopes: Vec<Value>) -> AnalysisResult {
    let mut input = String::new();
    for envelope in envelopes {
        input.push_str(&serde_json::to_string(&envelope).expect("fixture JSON"));
        input.push('\n');
    }
    analyze_reader(Cursor::new(input), AnalysisOptions::default()).expect("analysis succeeds")
}

mod sessions;
mod structure;

fn has_fact(result: &AnalysisResult, kind: DefectFactKind) -> bool {
    result
        .defects
        .iter()
        .flat_map(|defect| &defect.mismatch.facts)
        .any(|fact| fact.kind == kind)
}

fn has_content_fact(result: &AnalysisResult) -> bool {
    has_fact(result, DefectFactKind::ContentVariation)
        || has_fact(result, DefectFactKind::FixedVariants)
}

fn chat(messages: Vec<Value>) -> Value {
    json!({"model":"model-a","messages":messages})
}
