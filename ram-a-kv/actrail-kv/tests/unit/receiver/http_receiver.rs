//! 本文件验证 http_receiver 模块的行为。

use std::{fs, sync::Arc};

use actrail_kv_artifacts::CapturedRequest;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use tempfile::tempdir;
use tower::ServiceExt;

use crate::config::DEFAULT_MAX_PAYLOAD_BYTES as MAX_PAYLOAD_BYTES;
use crate::{router, NdjsonAppender};

#[tokio::test]
async fn accepts_object_and_preserves_payload_with_source() {
    let directory = tempdir().expect("create temporary directory");
    let output = directory.path().join("requests.ndjson");
    let app = router(Arc::new(
        NdjsonAppender::open(&output).expect("open appender"),
    ));
    let response = app
        .oneshot(
            Request::post("/requests")
                .header("content-type", "application/json")
                .header("X-Actrail-Source", "agent-hook")
                .header("X-Actrail-Session-Key", "session-001")
                .header("X-Actrail-Endpoint-Key", "llm-primary")
                .header("X-Actrail-Agent-Key", "coding-agent")
                .header("X-Actrail-Model-Deployment-Key", "deployment-a")
                .header("X-Actrail-KV-Namespace", "cache-scope-a")
                .body(Body::from(
                    r#"{"model":"example","messages":[{"content":"你好"}]}"#,
                ))
                .expect("build request"),
        )
        .await
        .expect("serve request");

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let line = fs::read_to_string(output).expect("read output");
    assert!(line.ends_with('\n'));
    assert_eq!(line.lines().count(), 1);
    let captured: CapturedRequest = serde_json::from_str(line.trim_end()).expect("parse line");
    assert_eq!(captured.source.as_deref(), Some("agent-hook"));
    assert_eq!(captured.session_key.as_deref(), Some("session-001"));
    assert_eq!(captured.comparison.endpoint_key, "llm-primary");
    assert_eq!(
        captured.comparison.agent_key.as_deref(),
        Some("coding-agent")
    );
    assert_eq!(
        captured.comparison.model_deployment_key.as_deref(),
        Some("deployment-a")
    );
    assert_eq!(
        captured.comparison.kv_namespace.as_deref(),
        Some("cache-scope-a")
    );
    assert_eq!(captured.payload["messages"][0]["content"], "你好");
    assert!(captured.captured_at.ends_with('Z'));
}

#[tokio::test]
async fn requires_explicit_endpoint_key() {
    let directory = tempdir().expect("create temporary directory");
    let output = directory.path().join("requests.ndjson");
    let app = router(Arc::new(
        NdjsonAppender::open(&output).expect("open appender"),
    ));
    let response = app
        .oneshot(
            Request::post("/requests")
                .body(Body::from(r#"{"model":"example"}"#))
                .expect("build request"),
        )
        .await
        .expect("serve request");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(fs::metadata(output).expect("output metadata").len(), 0);
}

#[tokio::test]
async fn rejects_malformed_json_and_all_non_object_json() {
    for body in ["{", "null", "[]", "\"text\"", "42", "true"] {
        let directory = tempdir().expect("create temporary directory");
        let output = directory.path().join("requests.ndjson");
        let app = router(Arc::new(
            NdjsonAppender::open(&output).expect("open appender"),
        ));
        let response = app
            .oneshot(
                Request::post("/requests")
                    .body(Body::from(body))
                    .expect("build request"),
            )
            .await
            .expect("serve request");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "body: {body}");
        assert_eq!(fs::metadata(output).expect("output metadata").len(), 0);
    }
}

#[tokio::test]
async fn rejects_body_over_limit_with_payload_too_large() {
    let directory = tempdir().expect("create temporary directory");
    let output = directory.path().join("requests.ndjson");
    let app = router(Arc::new(
        NdjsonAppender::open(&output).expect("open appender"),
    ));
    let body = format!(r#"{{"content":"{}"}}"#, "x".repeat(MAX_PAYLOAD_BYTES));
    let response = app
        .oneshot(
            Request::post("/requests")
                .body(Body::from(body))
                .expect("build request"),
        )
        .await
        .expect("serve request");

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(fs::metadata(output).expect("output metadata").len(), 0);
}

#[tokio::test]
async fn returns_method_not_allowed_for_non_post_requests() {
    let directory = tempdir().expect("create temporary directory");
    let app = router(Arc::new(
        NdjsonAppender::open(directory.path().join("requests.ndjson")).expect("open appender"),
    ));

    for method in [Method::GET, Method::PUT, Method::DELETE, Method::PATCH] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/requests")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("serve request");
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_http_requests_stay_as_one_ndjson_record_per_line() {
    const REQUESTS: usize = 200;

    let directory = tempdir().expect("create temporary directory");
    let output = directory.path().join("requests.ndjson");
    let app = router(Arc::new(
        NdjsonAppender::open(&output).expect("open appender"),
    ));
    let handles: Vec<_> = (0..REQUESTS)
        .map(|sequence| {
            let app = app.clone();
            tokio::spawn(async move {
                let response = app
                    .oneshot(
                        Request::post("/requests")
                            .header("X-Actrail-Endpoint-Key", "llm-primary")
                            .body(Body::from(format!(
                                r#"{{"sequence":{sequence},"content":"line\n{sequence}"}}"#
                            )))
                            .expect("build request"),
                    )
                    .await
                    .expect("serve request");
                assert_eq!(response.status(), StatusCode::ACCEPTED);
            })
        })
        .collect();

    for handle in handles {
        handle.await.expect("request task succeeds");
    }

    let contents = fs::read_to_string(output).expect("read output");
    let mut seen = vec![false; REQUESTS];
    assert_eq!(contents.lines().count(), REQUESTS);
    for line in contents.lines() {
        let captured: CapturedRequest = serde_json::from_str(line).expect("parse intact line");
        let sequence = captured.payload["sequence"].as_u64().expect("sequence") as usize;
        seen[sequence] = true;
    }
    assert!(seen.into_iter().all(|present| present));
}

#[cfg(unix)]
#[tokio::test]
async fn returns_internal_server_error_when_append_fails() {
    let app = router(Arc::new(
        NdjsonAppender::open("/dev/full").expect("open full device"),
    ));
    let response = app
        .oneshot(
            Request::post("/requests")
                .header("X-Actrail-Endpoint-Key", "llm-primary")
                .body(Body::from(r#"{"model":"example"}"#))
                .expect("build request"),
        )
        .await
        .expect("serve request");

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}
