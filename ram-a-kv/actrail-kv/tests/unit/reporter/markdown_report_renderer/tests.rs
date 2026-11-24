//! 本文件验证 markdown_report_renderer 模块的行为。

use actrail_kv_artifacts::{
    CacheMetricBasis, KvCacheMetric, PrefixNode, PrefixNodeKind, PrefixReuseComposition,
    PrefixReuseSession, PrefixReuseSummary, SessionEventType, SessionReport, SessionSwitchEvent,
    SourceLocation, TemplatePrefixView,
};

use crate::{
    locale::{Locale, LocaleSnapshot},
    render_html_with_locale, render_markdown_with_locale,
};

use super::render_markdown;

fn assert_gfm_tables_are_contiguous(markdown: &str) {
    let lines: Vec<_> = markdown.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        let is_separator = line.starts_with('|')
            && line.ends_with('|')
            && line.split('|').filter(|cell| !cell.is_empty()).all(|cell| {
                let cell = cell.trim();
                let cell = cell.trim_matches(':');
                cell.len() >= 3 && cell.chars().all(|character| character == '-')
            });
        if is_separator {
            let next = lines
                .get(index + 1)
                .unwrap_or_else(|| panic!("table separator has no following row: {line}"));
            assert!(
                next.starts_with('|') && !next.is_empty(),
                "blank or non-table line follows GFM separator {line}: {next:?}"
            );
        }
    }
}

#[test]
fn renders_all_report_sections_as_pure_gfm() {
    let mut result = crate::result_loader::tests::fixture();
    result.prefix_reuse = Some(PrefixReuseSummary {
        sessions: 1,
        request_pairs: 2,
        lcp_bytes: 80,
        context_bytes: 100,
        ratio: 0.8,
        potential_lcp_bytes: 90,
        potential_ratio: 0.9,
        gain_bytes: 10,
        recoverable_stable_bytes: 0,
        sessions_with_prefix_cut: 0,
        by_session: vec![PrefixReuseSession {
            session_key: "session-a".into(),
            request_pairs: 2,
            lcp_bytes: 80,
            context_bytes: 100,
            ratio: 0.8,
            potential_lcp_bytes: 90,
            gain_bytes: 10,
        }],
        requests: vec![],
        composition: PrefixReuseComposition {
            context_bytes: 100,
            reusable_bytes: 80,
            recoverable_bytes: 10,
            new_content_bytes: 10,
        },
    });
    result.cache_metrics = vec![KvCacheMetric {
        request_id: "request-a".into(),
        session_key: Some("session-a".into()),
        sequence: 1,
        captured_at: None,
        source: None,
        prompt_tokens: Some(100),
        cached_tokens: Some(50),
        miss_tokens: Some(50),
        output_tokens: Some(10),
        hit_rate: 0.5,
        hit_rate_delta: None,
        basis: CacheMetricBasis::Reported,
        estimated_lcp_bytes: None,
        payload_bytes: 100,
        ttft_ms: Some(10),
        total_ms: Some(20),
    }];
    result.session_reports = vec![SessionReport {
        session_key: "session-a".into(),
        endpoint_key: "endpoint".into(),
        model: "model".into(),
        request_count: 2,
        append_count: 0,
        fork_count: 0,
        reorder_count: 0,
        reset_count: 1,
        context_refresh_count: 1,
        total_recomputed_bytes: 10,
        total_stable_after_switch_bytes: 5,
        total_prefix_cut_bytes: 3,
        avg_prefix_cut_bytes: 3,
        total_abandoned_prefix_bytes: 3,
        domain_change_count: 0,
        events: vec![SessionSwitchEvent {
            event_type: SessionEventType::Reset,
            previous_sequence: 1,
            next_sequence: 2,
            prev_request_id: "a".into(),
            next_request_id: "b".into(),
            lcp_units: 1,
            lcp_bytes: 2,
            previous_pair_lcp_bytes: 0,
            prefix_cut_bytes: 3,
            previous_total_bytes: 5,
            retained_prefix_bytes: 2,
            prefix_reuse_ratio: Some(0.4),
            abandoned_prefix_bytes: 3,
            prefix_reuse_outlier: true,
            domain_changed: false,
            next_total_bytes: 5,
            recomputed_bytes: 3,
            stable_after_switch_bytes: 1,
            next_block_runs: "abcdx1".into(),
        }],
    }];
    result.templates[0].prefix_view = Some(TemplatePrefixView {
        node_count: 2,
        truncated: false,
        nodes: vec![
            PrefixNode {
                id: "seq-0".into(),
                kind: PrefixNodeKind::Stable,
                source: SourceLocation {
                    json_path: "$.messages[0].content".into(),
                    logical_scope: vec!["messages".into()],
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
                kind: PrefixNodeKind::Stable,
                source: SourceLocation {
                    json_path: "$.messages[0].content".into(),
                    logical_scope: vec!["messages".into()],
                    unit_index: Some(0),
                    byte_start: Some(8),
                    byte_end: Some(12),
                    role: Some("system".into()),
                },
                utf8_bytes: 4,
                support_count: 3,
                support_ratio: 0.75,
                excerpt: "TAIL".into(),
                member_request_ids: vec!["a".into(), "b".into(), "c".into()],
                content_sample: None,
                sequence_child: None,
                internal_children: vec![],
                variants: None,
                defect_ids: vec![],
            },
        ],
    });
    result.session_reports[0].session_key = "<b>{ratio}</b>".into();
    let view = result.templates[0]
        .prefix_view
        .as_mut()
        .expect("prefix view");
    view.nodes[1].kind = PrefixNodeKind::Dynamic;
    view.nodes[1].excerpt = "<script>{ratio}</script>".into();

    let markdown = render_markdown(&result).expect("markdown");
    for heading in [
        "# LLM KV 缓存结构诊断",
        "## 结构复用率",
        "## Top K",
        "## 缺陷详情",
        "## 会话上下文刷新与前缀复用",
        "## KV 缓存命中率",
        "## 模板前缀树",
    ] {
        assert!(markdown.contains(heading), "missing {heading}: {markdown}");
    }
    for field in [
        "当前",
        "补完已知结构问题后的上界",
        "字节构成",
        "被阻断稳定字节",
        "P1",
        "X 变体",
        "P2 稳定证据",
        "首次刷新",
        "TTFT",
        "成员衰减",
        "结构骨架",
    ] {
        assert!(markdown.contains(field), "missing {field}: {markdown}");
    }
    for raw_html in ["<style", "<svg", "<details"] {
        assert!(!markdown.contains(raw_html), "raw HTML leaked: {raw_html}");
    }
    assert!(markdown.contains("&lt;b&gt;统一生成方式&lt;/b&gt;"));
    assert!(markdown.contains("`<script>alert(1)</script>`"));
    assert!(markdown.contains("#### 会话事件：`…<b>{ratio}</b>`"));
    assert!(markdown.contains("段「`<script>{ratio}</script>`」"));
    assert!(markdown.contains("### 缺陷 defect\n"));
    assert!(markdown.contains("[`defect`](#缺陷-defect)"));
    assert_gfm_tables_are_contiguous(&markdown);
}

#[test]
fn english_locale_translates_ui_but_preserves_analysis_insights() {
    let result = crate::result_loader::tests::fixture();
    let locale = LocaleSnapshot::new(Locale::EnUs);
    let html = render_html_with_locale(&result, locale).expect("english html");
    let markdown = render_markdown_with_locale(&result, locale).expect("english markdown");

    assert!(html.contains("<html lang=\"en-US\">"));
    assert!(html.contains("LLM KV Cache Structure Diagnostics"));
    assert!(html.contains("Defect Details"));
    assert!(!html.contains("缺陷详情"));
    assert!(html.contains("统一生成方式"));

    assert!(markdown.starts_with("# LLM KV Cache Structure Diagnostics"));
    assert!(markdown.contains("## Defect Details"));
    assert!(!markdown.contains("## 缺陷详情"));
    assert!(markdown.contains("&lt;b&gt;统一生成方式&lt;/b&gt;"));
    assert_gfm_tables_are_contiguous(&markdown);
}
