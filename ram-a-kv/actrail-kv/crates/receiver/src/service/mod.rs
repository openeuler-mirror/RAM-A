//! 将已校验采集输入转换为持久记录，并在后端写入完成后返回回执。
use crate::contracts::{CaptureError, CaptureInput, CaptureReceipt};
use actrail_kv_artifacts::CapturedRequest;
use actrail_kv_storage::CaptureStore;
use std::sync::Arc;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

pub struct CaptureService {
    store: Arc<dyn CaptureStore>,
}

impl CaptureService {
    pub(crate) fn new(store: Arc<dyn CaptureStore>) -> Self {
        Self { store }
    }

    pub async fn submit(&self, input: CaptureInput) -> Result<CaptureReceipt, CaptureError> {
        let captured_at = OffsetDateTime::now_utc().format(&Rfc3339)?;
        self.store
            .append(CapturedRequest {
                captured_at: captured_at.clone(),
                source: input.source,
                session_key: input.session_key,
                comparison: input.comparison,
                payload: input.payload,
            })
            .await?;
        Ok(CaptureReceipt { captured_at })
    }
}
