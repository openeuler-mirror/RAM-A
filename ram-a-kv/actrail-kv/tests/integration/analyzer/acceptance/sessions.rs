//! 本文件验收会话变化、排序和前缀复用。

use super::*;

#[test]
fn session_prefix_switch_reports_classify_append_fork_reorder_and_reset() {
    fn conversation(contents: &[&str]) -> Value {
        chat(
            contents
                .iter()
                .map(|content| json!({"role":"user","content":content}))
                .collect(),
        )
    }
    let payloads = vec![
        conversation(&["A1", "A2", "A3", "A4", "B1", "B2", "C1", "C2"]),
        conversation(&["A1", "A2", "A3", "A4", "B1", "B2", "C1", "C2", "D"]),
        conversation(&["A1", "A2", "A3", "A4", "E1", "E2"]),
        conversation(&["A1", "A2", "A3", "A4", "E2", "E1"]),
        conversation(&["X1", "X2", "X3"]),
    ];
    let result = analyze_with_session(payloads, Some("session-aaaabbcc"));
    assert_eq!(result.session_reports.len(), 1, "result={result:#?}");
    let report = &result.session_reports[0];
    assert_eq!(report.request_count, 5);
    let types: Vec<_> = report
        .events
        .iter()
        .map(|event| &event.event_type)
        .collect();
    assert!(
        types.contains(&&SessionEventType::Append),
        "report={report:#?}"
    );
    assert!(
        types.contains(&&SessionEventType::Fork),
        "report={report:#?}"
    );
    assert!(
        types.contains(&&SessionEventType::Reorder),
        "report={report:#?}"
    );
    assert!(
        types.contains(&&SessionEventType::Reset),
        "report={report:#?}"
    );
    assert!(report.total_recomputed_bytes > 0);
    assert!(report.total_stable_after_switch_bytes > 0);
    assert!(
        report.events.iter().any(|event| event.prefix_cut_bytes > 0),
        "report={report:#?}"
    );
    assert!(
        report
            .events
            .iter()
            .any(|event| !event.next_block_runs.is_empty()),
        "report={report:#?}"
    );
}

#[test]
fn session_context_refresh_counts_one_reset_and_resumes_append_only() {
    fn conversation(contents: &[&str]) -> Value {
        chat(
            contents
                .iter()
                .map(|content| json!({"role":"user","content":content}))
                .collect(),
        )
    }
    let payloads = [
        &["A"][..],
        &["A", "B"][..],
        &["A", "B", "C"][..],
        &["A", "B", "C", "D"][..],
        &["E", "F"][..],
        &["E", "F", "G"][..],
        &["E", "F", "G", "H"][..],
    ]
    .into_iter()
    .map(conversation)
    .collect();
    let result = analyze_with_session(payloads, Some("session-context-refresh"));
    let report = &result.session_reports[0];

    assert_eq!(report.context_refresh_count, 1);
    assert_eq!(report.total_abandoned_prefix_bytes, 4);
    let ratios: Vec<_> = report
        .events
        .iter()
        .map(|event| event.prefix_reuse_ratio.expect("measurable prefix"))
        .collect();
    assert_eq!(ratios, vec![1.0, 1.0, 1.0, 0.0, 1.0, 1.0]);
    let refresh = report
        .events
        .iter()
        .find(|event| event.prefix_reuse_outlier)
        .expect("refresh event");
    assert_eq!(refresh.event_type, SessionEventType::Reset);
    assert_eq!((refresh.previous_sequence, refresh.next_sequence), (4, 5));
    assert_eq!(refresh.previous_total_bytes, 4);
    assert_eq!(refresh.retained_prefix_bytes, 0);
    assert_eq!(refresh.abandoned_prefix_bytes, 4);
}

#[test]
fn append_only_session_has_no_context_refresh() {
    let payloads = vec![
        chat(vec![json!({"role":"user","content":"A"})]),
        chat(vec![
            json!({"role":"user","content":"A"}),
            json!({"role":"user","content":"B"}),
        ]),
        chat(vec![
            json!({"role":"user","content":"A"}),
            json!({"role":"user","content":"B"}),
            json!({"role":"user","content":"C"}),
        ]),
    ];
    let result = analyze_with_session(payloads, Some("session-append-only"));
    let report = &result.session_reports[0];

    assert_eq!(report.context_refresh_count, 0);
    assert_eq!(report.total_abandoned_prefix_bytes, 0);
    assert!(report
        .events
        .iter()
        .all(|event| !event.prefix_reuse_outlier));
}

#[test]
fn session_orders_requests_by_capture_time_instead_of_input_order() {
    let payload = |messages: Vec<Value>| {
        json!({
            "captured_at":"2026-01-01T00:00:00Z",
            "session_key":"session-time-order",
            "comparison":{"endpoint_key":"primary"},
            "payload":chat(messages)
        })
    };
    let mut last = payload(vec![
        json!({"role":"user","content":"A"}),
        json!({"role":"user","content":"B"}),
        json!({"role":"user","content":"C"}),
    ]);
    last["captured_at"] = json!("2026-01-01T00:00:03Z");
    let mut middle = payload(vec![
        json!({"role":"user","content":"A"}),
        json!({"role":"user","content":"B"}),
    ]);
    middle["captured_at"] = json!("2026-01-01T00:00:02Z");
    let mut first = payload(vec![json!({"role":"user","content":"A"})]);
    first["captured_at"] = json!("2026-01-01T00:00:01Z");

    let result = analyze_envelopes(vec![last, middle, first]);
    let report = &result.session_reports[0];
    assert_eq!(report.context_refresh_count, 0);
    assert!(report
        .events
        .iter()
        .all(|event| event.event_type == SessionEventType::Append));
}

#[test]
fn session_survives_model_change_without_splitting_report() {
    let payload = |model: &str, messages: Vec<Value>| {
        json!({
            "captured_at":"2026-01-01T00:00:00Z",
            "session_key":"session-model-change",
            "comparison":{"endpoint_key":"primary"},
            "payload":{"model":model,"messages":messages}
        })
    };
    let result = analyze_envelopes(vec![
        payload("model-a", vec![json!({"role":"user","content":"A"})]),
        payload(
            "model-b",
            vec![
                json!({"role":"user","content":"A"}),
                json!({"role":"user","content":"B"}),
            ],
        ),
    ]);

    assert_eq!(result.session_reports.len(), 1);
    let report = &result.session_reports[0];
    assert_eq!(report.request_count, 2);
    assert_eq!(report.domain_change_count, 1);
    assert!(report.events[0].domain_changed);
}

#[test]
fn projection_skip_does_not_shift_session_sequences() {
    let envelope = |captured_at: &str, payload: Value| {
        json!({
            "captured_at":captured_at,
            "session_key":"session-projection-skip",
            "comparison":{"endpoint_key":"primary"},
            "payload":payload
        })
    };
    let conversation = |contents: &[&str]| {
        chat(
            contents
                .iter()
                .map(|content| json!({"role":"user","content":content}))
                .collect(),
        )
    };
    let result = analyze_envelopes(vec![
        envelope("2026-01-01T00:00:00Z", json!({"model":"model-a"})),
        envelope("2026-01-01T00:00:01Z", conversation(&["A"])),
        envelope("2026-01-01T00:00:02Z", conversation(&["A", "B"])),
        envelope("2026-01-01T00:00:03Z", conversation(&["A", "B", "C"])),
        envelope("2026-01-01T00:00:04Z", conversation(&["A", "B", "C", "D"])),
    ]);

    let report = &result.session_reports[0];
    assert_eq!(report.request_count, 4);
    assert_eq!(report.events.len(), 3);
    let last_request_id = result
        .cache_metrics
        .iter()
        .find(|metric| {
            metric.session_key.as_deref() == Some("session-projection-skip") && metric.sequence == 5
        })
        .expect("last session request metric")
        .request_id
        .clone();
    assert_eq!(
        report.events.last().expect("last event").next_request_id,
        last_request_id
    );
    assert_eq!(report.context_refresh_count, 0);
}

#[test]
fn prefix_reuse_summarizes_session_pairs_with_structural_upper_bound() {
    fn conversation(contents: &[&str]) -> Value {
        chat(
            contents
                .iter()
                .map(|content| json!({"role":"user","content":content}))
                .collect(),
        )
    }
    let payloads = vec![
        conversation(&["A1", "A2", "A3"]),
        conversation(&["A1", "A2", "A3", "B1"]),
        conversation(&["A1", "A2", "A3", "B1", "C1"]),
    ];
    let result = analyze_with_session(payloads, Some("session-reuse"));
    let summary = result.prefix_reuse.as_ref().expect("prefix_reuse summary");

    // 三个请求构成两个相邻请求对；会话首个请求没有前序，不参与统计。
    assert_eq!(summary.sessions, 1);
    assert_eq!(summary.request_pairs, 2);
    assert_eq!(summary.by_session.len(), 1);
    assert_eq!(
        summary.by_session[0].lcp_bytes,
        result.session_reports[0]
            .events
            .iter()
            .map(|event| event.lcp_bytes)
            .sum::<usize>()
    );
    assert_eq!(
        summary.lcp_bytes,
        summary
            .by_session
            .iter()
            .map(|session| session.lcp_bytes)
            .sum::<usize>()
    );
    assert_eq!(
        summary.context_bytes,
        summary
            .by_session
            .iter()
            .map(|session| session.context_bytes)
            .sum::<usize>()
    );
    // 复用率与上界都必须在 [0,1] 内，且上界不低于当前值。
    assert!((0.0..=1.0).contains(&summary.ratio));
    assert!((0.0..=1.0).contains(&summary.potential_ratio));
    assert!(summary.potential_lcp_bytes >= summary.lcp_bytes);
    assert!(summary.potential_ratio >= summary.ratio);
    assert_eq!(
        summary.gain_bytes,
        summary.potential_lcp_bytes - summary.lcp_bytes
    );
    // 逐请求上限不得低于其当前复用前缀，也不得超过该请求上下文长度。
    for request in &summary.requests {
        assert!(request.potential_prefix_bytes >= request.reusable_prefix_bytes);
        assert!(request.gain_bytes > 0);
    }
    // 构成必须自洽：已复用 + 已知可恢复 + 每轮新增 == 上下文总字节。
    let composition = &summary.composition;
    assert_eq!(composition.context_bytes, summary.context_bytes);
    assert_eq!(composition.reusable_bytes, summary.lcp_bytes);
    assert_eq!(composition.recoverable_bytes, summary.gain_bytes);
    assert_eq!(
        composition.reusable_bytes + composition.recoverable_bytes + composition.new_content_bytes,
        composition.context_bytes
    );
}

#[test]
fn prefix_reuse_is_absent_without_session_pairs() {
    let result = analyze(vec![chat(vec![json!({"role":"user","content":"single"})])]);
    assert!(
        result.prefix_reuse.is_none(),
        "单请求没有可比较的相邻对，不应产出 prefix_reuse"
    );
}
