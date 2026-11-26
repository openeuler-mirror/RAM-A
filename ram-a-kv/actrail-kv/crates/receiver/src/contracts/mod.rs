//! 公开通道无关的采集输入、字段校验、回执和服务构造入口。
use actrail_kv_artifacts::ComparisonMetadata;
use actrail_kv_storage::CaptureStore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

/// 尚未归一化的通道输入，字段规则由采集工厂统一执行。
#[derive(Debug, Deserialize)]
pub struct CaptureRequest {
    pub payload: Value,
    pub source: Option<String>,
    pub session_key: Option<String>,
    pub comparison: CaptureComparison,
}

#[derive(Debug, Default, Deserialize)]
pub struct CaptureComparison {
    pub endpoint_key: Option<String>,
    pub agent_key: Option<String>,
    pub model_deployment_key: Option<String>,
    pub kv_namespace: Option<String>,
}

/// 只能通过公共校验入口或 HTTP 适配器构造的已校验输入。
pub struct CaptureInput {
    pub(crate) payload: Value,
    pub(crate) source: Option<String>,
    pub(crate) session_key: Option<String>,
    pub(crate) comparison: ComparisonMetadata,
}

#[derive(Debug, Serialize)]
pub struct CaptureReceipt {
    pub captured_at: String,
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureInputError {
    #[error("payload must be one JSON object")]
    InvalidPayload,
    #[error("comparison.endpoint_key is required")]
    MissingEndpoint,
    #[error("metadata must not be empty")]
    EmptyMetadata,
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("{kind}")]
    Input {
        field: &'static str,
        kind: CaptureInputError,
    },
    #[error("failed to construct capture timestamp: {0}")]
    Timestamp(#[from] time::error::Format),
    #[error("{0}")]
    Storage(#[from] actrail_kv_storage::StorageError),
}

pub use crate::service::CaptureService;

pub struct CaptureFactory;
impl CaptureFactory {
    pub fn create(store: Arc<dyn CaptureStore>) -> CaptureService {
        CaptureService::new(store)
    }

    pub fn input(request: CaptureRequest) -> Result<CaptureInput, CaptureError> {
        let payload = Self::payload(request.payload)?;
        let session_key = Self::metadata(request.session_key, "session_key", false)?;
        let comparison = request.comparison;
        let endpoint_key =
            Self::metadata(comparison.endpoint_key, "comparison.endpoint_key", true)?
                .expect("required metadata was validated");
        Ok(CaptureInput {
            payload,
            source: request.source,
            session_key,
            comparison: ComparisonMetadata {
                endpoint_key,
                agent_key: Self::metadata(comparison.agent_key, "comparison.agent_key", false)?,
                model_deployment_key: Self::metadata(
                    comparison.model_deployment_key,
                    "comparison.model_deployment_key",
                    false,
                )?,
                kv_namespace: Self::metadata(
                    comparison.kv_namespace,
                    "comparison.kv_namespace",
                    false,
                )?,
            },
        })
    }

    pub(crate) fn payload(payload: Value) -> Result<Value, CaptureError> {
        // 保留既有对象约束，所有通道共享同一规则。
        if !payload.is_object() {
            return Err(CaptureError::Input {
                field: "payload",
                kind: CaptureInputError::InvalidPayload,
            });
        }
        Ok(payload)
    }

    pub(crate) fn metadata(
        value: Option<String>,
        field: &'static str,
        required: bool,
    ) -> Result<Option<String>, CaptureError> {
        let Some(value) = value else {
            return if required {
                Err(CaptureError::Input {
                    field,
                    kind: CaptureInputError::MissingEndpoint,
                })
            } else {
                Ok(None)
            };
        };
        let value = value.trim();
        if value.is_empty() {
            return Err(CaptureError::Input {
                field,
                kind: CaptureInputError::EmptyMetadata,
            });
        }
        Ok(Some(value.to_owned()))
    }
}
