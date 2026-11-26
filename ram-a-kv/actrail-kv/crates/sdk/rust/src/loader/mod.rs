//! 装载受信任的 ABI 兼容库，并使库的生命周期覆盖实例和输出。
mod instance;
mod output;
use crate::{Result, SdkError};
use actrail_kv_sdk_contracts::*;
pub(crate) use instance::Instance;
use libloading::Library;
pub(crate) use output::Output;
use std::sync::Arc;

pub(crate) enum Operation {
    Call(CallFn),
    Render(RenderFn),
}
pub(crate) struct Api {
    // 保持 Library 存活至最后一个实例、输出和函数指针释放。
    _library: Library,
    pub create: CreateFn,
    pub destroy: DestroyFn,
    pub free: FreeFn,
    pub operation: Operation,
    pub export: Option<ExportFn>,
}
impl Api {
    pub fn load(path: &std::path::Path, component: &str) -> Result<Arc<Self>> {
        // SAFETY: SDK 仅加载用户显式选择且遵守公开 ABI 的受信任动态库。
        unsafe {
            let library = Library::new(path).map_err(|e| SdkError::Load(e.to_string()))?;
            let symbol = |name: &str| format!("actrail_kv_{component}_{name}");
            let major: VersionFn = *library
                .get(symbol("abi_major").as_bytes())
                .map_err(|e| SdkError::Load(e.to_string()))?;
            let minor: VersionFn = *library
                .get(symbol("abi_minor").as_bytes())
                .map_err(|e| SdkError::Load(e.to_string()))?;
            let (major, minor) = (major(), minor());
            if major != ABI_MAJOR || (major, minor) < (ABI_MAJOR, ABI_MINOR) {
                return Err(SdkError::Abi(format!(
                    "expected {ABI_MAJOR}.{ABI_MINOR}+, found {major}.{minor}"
                )));
            }
            let create = *library
                .get::<CreateFn>(symbol("create").as_bytes())
                .map_err(|e| SdkError::Load(e.to_string()))?;
            let destroy = *library
                .get::<DestroyFn>(symbol("destroy").as_bytes())
                .map_err(|e| SdkError::Load(e.to_string()))?;
            let free = *library
                .get::<FreeFn>(symbol("buffer_free").as_bytes())
                .map_err(|e| SdkError::Load(e.to_string()))?;
            let operation = if component == "reporter" {
                Operation::Render(
                    *library
                        .get::<RenderFn>(symbol("render").as_bytes())
                        .map_err(|e| SdkError::Load(e.to_string()))?,
                )
            } else {
                let name = if component == "receiver" {
                    "submit"
                } else {
                    "analyze"
                };
                Operation::Call(
                    *library
                        .get::<CallFn>(symbol(name).as_bytes())
                        .map_err(|e| SdkError::Load(e.to_string()))?,
                )
            };
            let export = if component == "receiver" {
                None
            } else {
                Some(
                    *library
                        .get::<ExportFn>(symbol("export").as_bytes())
                        .map_err(|e| SdkError::Load(e.to_string()))?,
                )
            };
            Ok(Arc::new(Self {
                _library: library,
                create,
                destroy,
                free,
                operation,
                export,
            }))
        }
    }
}
