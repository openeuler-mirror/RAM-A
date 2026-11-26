//! 导出直接采集 C ABI，在专用运行时内持续持久化请求。
use actrail_kv_receiver::contracts::{
    CaptureError, CaptureFactory, CaptureRequest, CaptureService,
};
use actrail_kv_sdk_runtime::{abi::*, Application, Boundary, Config, Error, Result, Runtime};
use actrail_kv_storage::{open_store, StorageConfig, StoreAccess};
use serde_json::Value;

struct Receiver {
    service: CaptureService,
}
impl Application for Receiver {
    fn create(mut config: Config, runtime: &Runtime) -> Result<Self> {
        let storage = config
            .section::<StorageConfig>("storage")?
            .resolve(&config.base_dir)
            .map_err(Error::config)?;
        config.finish()?;
        let store = runtime
            .block_on(open_store(storage, StoreAccess::ReadWrite))
            .map_err(Error::storage)?;
        Ok(Self {
            service: CaptureFactory::create(store),
        })
    }
    fn call(&mut self, runtime: &Runtime, input: &[u8], _options: &[u8]) -> Result<Value> {
        let value = actrail_kv_sdk_runtime::request(input)?;
        let request: CaptureRequest = serde_json::from_value(value).map_err(Error::input)?;
        let input = CaptureFactory::input(request).map_err(capture_error)?;
        let receipt = runtime
            .block_on(self.service.submit(input))
            .map_err(capture_error)?;
        serde_json::to_value(receipt).map_err(Error::internal)
    }
}
fn capture_error(error: CaptureError) -> Error {
    match error {
        CaptureError::Input { field, kind } => {
            let mut error = Error::new(3, "capture_input", kind);
            error.detail.field = Some(field.into());
            error
        }
        CaptureError::Storage(error) => Error::storage(error),
        CaptureError::Timestamp(error) => Error::internal(error),
    }
}
actrail_kv_sdk_runtime::export_lifecycle!(
    Receiver,
    actrail_kv_receiver_abi_major,
    actrail_kv_receiver_abi_minor,
    actrail_kv_receiver_create,
    actrail_kv_receiver_destroy,
    actrail_kv_receiver_buffer_free
);
/// # Safety
/// 句柄和缓冲区须符合 SDK C ABI 契约，操作不得与销毁并发。
#[no_mangle]
pub unsafe extern "C" fn actrail_kv_receiver_submit(
    handle: Handle,
    input: InputBuffer,
    output: *mut OutputBuffer,
) -> i32 {
    unsafe { Boundary::<Receiver>::call(handle, input, InputBuffer::new(&[]), output) }
}
