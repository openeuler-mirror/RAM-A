//! 独占管理原生实例句柄，确保操作串行并在销毁后释放动态库引用。
use super::{Api, Operation, Output};
use crate::{Result, SdkConfig};
use actrail_kv_sdk_contracts::{Handle, InputBuffer, OutputBuffer, SCHEMA_VERSION};
use serde_json::{json, Value};
use std::{cell::Cell, marker::PhantomData, path::PathBuf, sync::Arc};

pub(crate) struct Instance {
    api: Arc<Api>,
    handle: Handle,
    base_dir: PathBuf,
    _exclusive: PhantomData<Cell<()>>,
}
// SAFETY: 句柄的全部操作要求独占访问，线程状态封装在库内工作线程。
unsafe impl Send for Instance {}
impl Instance {
    pub fn create(config: SdkConfig, component: &str) -> Result<Self> {
        let path = std::fs::canonicalize(&config.library_path)?;
        let api = Api::load(&path, component)?;
        let input = serde_json::to_vec(
            &json!({"schema_version": SCHEMA_VERSION, "base_dir": config.base_dir, "config_toml": config.config_toml, "overrides": config.overrides}),
        )?;
        let mut handle = std::ptr::null_mut();
        let mut buffer = OutputBuffer::default();
        // SAFETY: 输入切片在调用内存活，两个输出槽独立且可写。
        let status = unsafe { (api.create)(InputBuffer::new(&input), &mut handle, &mut buffer) };
        let instance = Self {
            api: api.clone(),
            handle,
            base_dir: config.base_dir.clone(),
            _exclusive: PhantomData,
        };
        Output::new(api, buffer, config.base_dir, status)?;
        if instance.handle.is_null() {
            return Err(crate::SdkError::Protocol(
                "create succeeded with null handle".into(),
            ));
        }
        Ok(instance)
    }
    pub fn call(&mut self, request: &Value) -> Result<Output> {
        let input = serde_json::to_vec(request)?;
        let mut output = OutputBuffer::default();
        let Operation::Call(call) = self.api.operation else {
            unreachable!("component operation")
        };
        // SAFETY: self 独占句柄，输入在阻塞调用期间有效。
        let status = unsafe { call(self.handle, InputBuffer::new(&input), &mut output) };
        Output::new(self.api.clone(), output, self.base_dir.clone(), status)
    }
    pub fn render(&mut self, analysis: &Output, options: &Value) -> Result<Output> {
        let options = serde_json::to_vec(options)?;
        let mut output = OutputBuffer::default();
        let Operation::Render(render) = self.api.operation else {
            unreachable!("component operation")
        };
        // SAFETY: analysis 的借用覆盖整个调用，原库和缓冲区均保持存活。
        let status = unsafe {
            render(
                self.handle,
                InputBuffer::new(analysis.bytes()),
                InputBuffer::new(&options),
                &mut output,
            )
        };
        Output::new(self.api.clone(), output, self.base_dir.clone(), status)
    }
    pub fn close(mut self) -> Result<()> {
        self.destroy()
    }
    fn destroy(&mut self) -> Result<()> {
        let handle = std::mem::replace(&mut self.handle, std::ptr::null_mut());
        if handle.is_null() {
            return Ok(());
        }
        let mut output = OutputBuffer::default();
        // SAFETY: 句柄由本实例独占，先置空保证仅销毁一次。
        let status = unsafe { (self.api.destroy)(handle, &mut output) };
        Output::new(self.api.clone(), output, self.base_dir.clone(), status)?;
        Ok(())
    }
}
impl Drop for Instance {
    fn drop(&mut self) {
        let _ = self.destroy();
    }
}
