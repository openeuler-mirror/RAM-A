//! 本文件验证 analysis_result/session 模块的行为。

use super::{SessionEventType, SessionReport};

#[test]
fn legacy_session_report_defaults_refresh_fields() {
    let encoded = r#"{
          "session_key":"legacy",
          "endpoint_key":"primary",
          "model":"model-a",
          "request_count":2,
          "append_count":0,
          "fork_count":0,
          "reorder_count":0,
          "reset_count":1,
          "total_recomputed_bytes":10,
          "total_stable_after_switch_bytes":0,
          "total_prefix_cut_bytes":4,
          "avg_prefix_cut_bytes":4,
          "events":[{
            "event_type":"reset",
            "prev_request_id":"a",
            "next_request_id":"b",
            "lcp_units":0,
            "lcp_bytes":0,
            "previous_pair_lcp_bytes":4,
            "prefix_cut_bytes":4,
            "next_total_bytes":10,
            "recomputed_bytes":10,
            "stable_after_switch_bytes":0,
            "next_block_runs":""
          }]
        }"#;
    let report: SessionReport = serde_json::from_str(encoded).expect("legacy report");

    assert_eq!(report.context_refresh_count, 0);
    assert_eq!(report.total_abandoned_prefix_bytes, 0);
    assert_eq!(report.domain_change_count, 0);
    assert_eq!(report.events[0].event_type, SessionEventType::Reset);
    assert_eq!(report.events[0].previous_total_bytes, 0);
    assert_eq!(report.events[0].retained_prefix_bytes, 0);
    assert!(report.events[0].prefix_reuse_ratio.is_none());
    assert!(!report.events[0].prefix_reuse_outlier);
}
