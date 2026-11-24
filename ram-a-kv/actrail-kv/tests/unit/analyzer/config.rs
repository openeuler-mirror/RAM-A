//! 本文件验证 config 模块的行为。

use super::*;

#[test]
fn json_round_trip_preserves_defaults() {
    let config = AnalyzeConfig::default();
    let json = config.to_json_pretty().expect("serialize");
    let parsed = AnalyzeConfig::from_json_str(&json).expect("parse");
    assert_eq!(parsed, config);
}

#[test]
fn rejects_unknown_fields() {
    let json = r#"{"top_k": 20, "unknown_field": 1}"#;
    assert!(AnalyzeConfig::from_json_str(json).is_err());
}
