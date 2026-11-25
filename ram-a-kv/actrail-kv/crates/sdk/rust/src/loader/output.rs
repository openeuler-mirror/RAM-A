//! 持有原生完整响应，提供借用数据视图和独立于业务实例的显式导出。
use super::Api;
use crate::{Result, SdkError};
use actrail_kv_sdk_contracts::{ErrorDetail, InputBuffer, OutputBuffer, SCHEMA_VERSION};
use serde::Deserialize;
use serde_json::{json, value::RawValue};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Deserialize)]
struct ResponseView<'a> {
    schema_version: u32,
    ok: bool,
    #[serde(borrow)]
    data: Option<&'a RawValue>,
    error: Option<ErrorDetail>,
}
pub(crate) struct Output {
    api: Arc<Api>,
    buffer: OutputBuffer,
    base_dir: PathBuf,
}
// SAFETY: 输出字节不可变，所有权唯一，原库分配器支持跨线程释放。
unsafe impl Send for Output {}
// SAFETY: 共享访问只产生只读切片，析构必须取得独占所有权。
unsafe impl Sync for Output {}
impl Output {
    pub fn new(
        api: Arc<Api>,
        buffer: OutputBuffer,
        base_dir: PathBuf,
        status: i32,
    ) -> Result<Self> {
        let output = Self {
            api,
            buffer,
            base_dir,
        };
        if output.buffer.ptr.is_null() || output.buffer.len > isize::MAX as usize {
            return Err(SdkError::Protocol("invalid output buffer".into()));
        }
        let view: ResponseView<'_> = serde_json::from_slice(output.bytes())?;
        if view.schema_version != SCHEMA_VERSION || view.ok != (status == 0) {
            return Err(SdkError::Protocol(
                "response version or status mismatch".into(),
            ));
        }
        if let Some(detail) = view.error {
            return Err(SdkError::Operation { status, detail });
        }
        if !view.ok || view.data.is_none() {
            return Err(SdkError::Protocol(
                "response is missing data or error".into(),
            ));
        }
        Ok(output)
    }
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: 已加载的 ABI 实现保证分配布局，Output 持有原库直到释放。
        unsafe { std::slice::from_raw_parts(self.buffer.ptr, self.buffer.len) }
    }
    pub fn data(&self) -> Result<&RawValue> {
        let view: ResponseView<'_> = serde_json::from_slice(self.bytes())?;
        view.data
            .ok_or_else(|| SdkError::Protocol("missing data".into()))
    }
    pub fn export(&self, path: &Path, format: Option<&str>) -> Result<()> {
        let export = self
            .api
            .export
            .ok_or_else(|| SdkError::Protocol("output is not exportable".into()))?;
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            self.base_dir.join(path)
        };
        let destination = serde_json::to_vec(
            &json!({"schema_version": SCHEMA_VERSION, "path": path, "format": format}),
        )?;
        let mut buffer = OutputBuffer::default();
        // SAFETY: 两个借用缓冲区在原生函数返回前有效。
        let status = unsafe {
            export(
                InputBuffer::new(self.bytes()),
                InputBuffer::new(&destination),
                &mut buffer,
            )
        };
        Self::new(self.api.clone(), buffer, self.base_dir.clone(), status)?;
        Ok(())
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        let buffer = std::mem::take(&mut self.buffer);
        // SAFETY: 完整缓冲区交还分配库恰好一次，随后才释放库引用。
        unsafe {
            (self.api.free)(buffer);
        }
    }
}
