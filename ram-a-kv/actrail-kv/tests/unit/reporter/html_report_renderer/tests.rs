//! 本文件回归验证编译期 HTML 模板的主区块、空态、会话分组与锚点。

use actrail_kv_artifacts::{
    CacheMetricBasis, KvCacheMetric, PrefixNode, PrefixNodeKind, PrefixReuseSummary,
    SessionEventType, SessionReport, SessionSwitchEvent, TemplatePrefixView,
};

use super::{render_html, view::session};

fn prefix_reuse() -> PrefixReuseSummary {
    PrefixReuseSummary {
        sessions: 2,
        request_pairs: 8,
        lcp_bytes: 840,
        context_bytes: 1000,
        ratio: 0.84,
        potential_lcp_bytes: 930,
        potential_ratio: 0.93,
        gain_bytes: 90,
        recoverable_stable_bytes: 0,
        sessions_with_prefix_cut: 0,
        by_session: vec![actrail_kv_artifacts::PrefixReuseSession {
            session_key: "session-a".to_string(),
            request_pairs: 4,
            lcp_bytes: 420,
            context_bytes: 500,
            ratio: 0.84,
            potential_lcp_bytes: 465,
            gain_bytes: 45,
        }],
        requests: vec![],
        composition: actrail_kv_artifacts::PrefixReuseComposition {
            context_bytes: 1000,
            reusable_bytes: 840,
            recoverable_bytes: 90,
            new_content_bytes: 70,
        },
    }
}

fn cache_metric() -> KvCacheMetric {
    KvCacheMetric {
        request_id: "session-a:0".into(),
        session_key: Some("session-a".into()),
        sequence: 1,
        captured_at: None,
        source: None,
        prompt_tokens: Some(1000),
        cached_tokens: Some(700),
        miss_tokens: Some(300),
        output_tokens: Some(10),
        hit_rate: 0.7,
        hit_rate_delta: None,
        basis: CacheMetricBasis::Reported,
        estimated_lcp_bytes: None,
        payload_bytes: 1200,
        ttft_ms: None,
        total_ms: None,
    }
}

fn session_report(key: &str, event_type: SessionEventType) -> SessionReport {
    let append = matches!(event_type, SessionEventType::Append);
    SessionReport {
        session_key: key.into(),
        endpoint_key: "endpoint-a".into(),
        model: "model-a".into(),
        request_count: 2,
        append_count: usize::from(append),
        fork_count: 0,
        reorder_count: 0,
        reset_count: usize::from(!append),
        context_refresh_count: usize::from(!append),
        total_recomputed_bytes: 100,
        total_stable_after_switch_bytes: 4,
        total_prefix_cut_bytes: usize::from(!append) * 100,
        avg_prefix_cut_bytes: usize::from(!append) * 100,
        total_abandoned_prefix_bytes: usize::from(!append) * 100,
        domain_change_count: 0,
        events: vec![SessionSwitchEvent {
            event_type,
            previous_sequence: 1,
            next_sequence: 2,
            prev_request_id: "prev-request-id".into(),
            next_request_id: "next-request-id".into(),
            lcp_units: 3,
            lcp_bytes: 900,
            previous_pair_lcp_bytes: 0,
            prefix_cut_bytes: usize::from(!append) * 100,
            previous_total_bytes: 1000,
            retained_prefix_bytes: if append { 1000 } else { 900 },
            prefix_reuse_ratio: Some(if append { 1.0 } else { 0.9 }),
            abandoned_prefix_bytes: usize::from(!append) * 100,
            prefix_reuse_outlier: !append,
            domain_changed: false,
            next_total_bytes: 1000,
            recomputed_bytes: 100,
            stable_after_switch_bytes: 4,
            next_block_runs: "abcdx1".into(),
        }],
    }
}

fn prefix_view() -> TemplatePrefixView {
    TemplatePrefixView {
        node_count: 2,
        truncated: false,
        nodes: vec![
            PrefixNode {
                id: "seq-0".into(),
                kind: PrefixNodeKind::Stable,
                source: actrail_kv_artifacts::SourceLocation {
                    json_path: "$.messages[0].content".into(),
                    logical_scope: vec![],
                    unit_index: Some(0),
                    byte_start: Some(0),
                    byte_end: Some(8),
                    role: Some("system".into()),
                },
                utf8_bytes: 8,
                support_count: 4,
                support_ratio: 1.0,
                excerpt: "PREFIX".into(),
                member_request_ids: vec!["a".into(), "b".into(), "c".into(), "d".into()],
                content_sample: Some("PREFIX-sample".into()),
                sequence_child: Some("seq-1".into()),
                internal_children: vec![],
                variants: None,
                defect_ids: vec![],
            },
            PrefixNode {
                id: "seq-1".into(),
                kind: PrefixNodeKind::Dynamic,
                source: actrail_kv_artifacts::SourceLocation {
                    json_path: "$.messages[0].content".into(),
                    logical_scope: vec![],
                    unit_index: Some(0),
                    byte_start: Some(8),
                    byte_end: Some(9),
                    role: Some("system".into()),
                },
                utf8_bytes: 1,
                support_count: 4,
                support_ratio: 1.0,
                excerpt: "1|2|3".into(),
                member_request_ids: vec!["a".into(), "b".into(), "c".into(), "d".into()],
                content_sample: None,
                sequence_child: None,
                internal_children: vec![],
                variants: Some(vec![actrail_kv_artifacts::PrefixVariant {
                    excerpt: "1".into(),
                    count: 2,
                    count_known: true,
                }]),
                defect_ids: vec!["defect".into()],
            },
        ],
    }
}

#[test]
fn renders_all_report_sections_through_embedded_templates() {
    let mut result = crate::result_loader::tests::fixture();
    result.prefix_reuse = Some(prefix_reuse());
    result.cache_metrics = vec![cache_metric()];
    result.session_reports = vec![session_report("batch-1-reset-01", SessionEventType::Reset)];
    result.templates[0].prefix_view = Some(prefix_view());

    let html = render_html(&result).expect("render html");
    for fragment in [
        "<!doctype html>",
        "<html lang=\"zh-CN\">",
        "结构代理 vs 真实命中",
        "上下文刷新警告（1 个会话）",
        "KV 缓存命中率",
        "模板前缀树",
        "href=\"#defect-defect\"",
        "id=\"defect-defect\"",
        "#defect-defect",
        "PREFIX-sample",
    ] {
        assert!(html.contains(fragment), "missing fragment: {fragment}");
    }
    assert!(!html.contains("{{"));
    assert!(!html.contains("{%"));
}

#[test]
fn omits_optional_sections_and_renders_top_k_empty_state() {
    let mut result = crate::result_loader::tests::fixture();
    result.prefix_reuse = None;
    result.cache_metrics.clear();
    result.session_reports.clear();
    result.defects.clear();
    result.top_k.clear();
    result.templates[0].prefix_view = None;

    let html = render_html(&result).expect("render empty html");
    assert!(html.contains("本次未发现上下文结构缺陷"));
    for absent in [
        "结构复用率",
        "KV 缓存命中率",
        "会话上下文刷新与前缀复用",
        "模板前缀树",
    ] {
        assert!(!html.contains(absent), "unexpected section: {absent}");
    }
}

#[test]
fn first_refresh_pair_uses_lowest_sequence_and_supports_legacy_events() {
    let append = session_report("append", SessionEventType::Append);
    assert_eq!(session::first_refresh_pair(&append), None);

    let mut refresh = session_report("refresh", SessionEventType::Reset);
    refresh.events[0].previous_sequence = 7;
    refresh.events[0].next_sequence = 8;
    let mut later = refresh.events[0].clone();
    later.previous_sequence = 3;
    later.next_sequence = 4;
    refresh.events.push(later);
    assert_eq!(session::first_refresh_pair(&refresh), Some((3, 4)));

    let mut legacy = session_report("legacy", SessionEventType::Fork);
    legacy.events[0].previous_sequence = 0;
    legacy.events[0].next_sequence = 0;
    legacy.events[0].prefix_reuse_ratio = None;
    legacy.events[0].prefix_reuse_outlier = false;
    assert_eq!(session::first_refresh_pair(&legacy), Some((1, 2)));
}

#[test]
fn cyclic_prefix_view_returns_an_error_instead_of_hanging() {
    let mut result = crate::result_loader::tests::fixture();
    let mut view = prefix_view();
    view.nodes[0].sequence_child = Some("seq-0".into());
    result.templates[0].prefix_view = Some(view);

    let error = render_html(&result).expect_err("cyclic prefix view must fail");
    assert!(error.to_string().contains("cycle"));
}
