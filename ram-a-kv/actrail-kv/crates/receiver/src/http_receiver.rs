//! 本文件定义 POST /requests 的输入校验、采集元数据补全和状态码契约。

use std::sync::Arc;

use actrail_kv_artifacts::{CapturedRequest, ComparisonMetadata};
use actrail_kv_storage::CaptureStore;
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use serde_json::Value;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::config::DEFAULT_MAX_PAYLOAD_BYTES;
use crate::NdjsonAppender;

const SOURCE_HEADER: &str = "x-actrail-source";
const SESSION_KEY_HEADER: &str = "x-actrail-session-key";
const ENDPOINT_KEY_HEADER: &str = "x-actrail-endpoint-key";
const AGENT_KEY_HEADER: &str = "x-actrail-agent-key";
const MODEL_DEPLOYMENT_KEY_HEADER: &str = "x-actrail-model-deployment-key";
const KV_NAMESPACE_HEADER: &str = "x-actrail-kv-namespace";

pub fn router(appender: Arc<NdjsonAppender>) -> Router {
    router_with_limit(appender, DEFAULT_MAX_PAYLOAD_BYTES)
}

/// 使用自定义请求体上限构建路由，供配置文件覆盖默认 8 MiB 限制。
pub fn router_with_limit(appender: Arc<NdjsonAppender>, max_payload_bytes: usize) -> Router {
    router_with_storage(appender, max_payload_bytes)
}

pub fn router_with_storage(store: Arc<dyn CaptureStore>, max_payload_bytes: usize) -> Router {
    Router::new()
        .route("/requests", post(receive_request))
        .layer(DefaultBodyLimit::max(max_payload_bytes))
        .with_state(store)
}

async fn receive_request(
    State(store): State<Arc<dyn CaptureStore>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(Value::Object(object)) => Value::Object(object),
        Ok(_) | Err(_) => return client_error("body must be one valid JSON object"),
    };
    let source = match headers.get(SOURCE_HEADER) {
        Some(value) => match value.to_str() {
            Ok(value) => Some(value.to_owned()),
            Err(_) => return client_error("X-Actrail-Source must be valid header text"),
        },
        None => None,
    };
    let session_key = match metadata_header(&headers, SESSION_KEY_HEADER, false) {
        Ok(value) => value,
        Err(message) => return client_error(message),
    };
    let endpoint_key = match metadata_header(&headers, ENDPOINT_KEY_HEADER, true) {
        Ok(Some(value)) => value,
        Ok(None) => return client_error("X-Actrail-Endpoint-Key is required"),
        Err(message) => return client_error(message),
    };
    let agent_key = match metadata_header(&headers, AGENT_KEY_HEADER, false) {
        Ok(value) => value,
        Err(message) => return client_error(message),
    };
    let model_deployment_key = match metadata_header(&headers, MODEL_DEPLOYMENT_KEY_HEADER, false) {
        Ok(value) => value,
        Err(message) => return client_error(message),
    };
    let kv_namespace = match metadata_header(&headers, KV_NAMESPACE_HEADER, false) {
        Ok(value) => value,
        Err(message) => return client_error(message),
    };
    let captured_at = match OffsetDateTime::now_utc().format(&Rfc3339) {
        Ok(value) => value,
        Err(_) => return server_error(),
    };
    let record = CapturedRequest {
        captured_at,
        source,
        session_key,
        comparison: ComparisonMetadata {
            endpoint_key,
            agent_key,
            model_deployment_key,
            kv_namespace,
        },
        payload,
    };

    match store.append(record).await {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(error) => {
            eprintln!("failed to append captured request: {error}");
            server_error()
        }
    }
}

fn metadata_header(
    headers: &HeaderMap,
    name: &'static str,
    required: bool,
) -> Result<Option<String>, &'static str> {
    let Some(value) = headers.get(name) else {
        return if required {
            Err("X-Actrail-Endpoint-Key is required")
        } else {
            Ok(None)
        };
    };
    let value = value
        .to_str()
        .map_err(|_| "comparison metadata headers must contain valid text")?
        .trim();
    if value.is_empty() {
        return Err("comparison metadata headers must not be empty");
    }
    Ok(Some(value.to_owned()))
}

fn client_error(message: &'static str) -> Response {
    (StatusCode::BAD_REQUEST, message).into_response()
}

fn server_error() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "failed to persist captured request",
    )
        .into_response()
}

#[cfg(test)]
#[path = "../../../tests/unit/receiver/http_receiver.rs"]
mod tests;
