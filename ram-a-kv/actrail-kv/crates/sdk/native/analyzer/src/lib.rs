//! 导出按需分析和显式文件导出，不自动发布分析产物。
mod selection;
use actrail_kv_analyzer::run::{
    analyze_capture_stream_with_selection, analyze_reader_with_selection,
};
use actrail_kv_sdk_runtime::{abi::*, Application, Boundary, Config, Error, Result, Runtime};
use actrail_kv_storage::{open_store, StorageConfig, StoreAccess};
use analysis_contract::settings::AnalyzerSettings;
use serde_json::Value;
use std::{fs::File, io::BufReader};

struct Analyzer {
    config: Config,
    settings: AnalyzerSettings,
    source: actrail_kv_sdk_runtime::SourceProtection,
}
impl Application for Analyzer {
    fn create(mut config: Config, _runtime: &Runtime) -> Result<Self> {
        let settings: AnalyzerSettings = config.section("analyzer")?;
        let source = if let Some(input) = &settings.input {
            actrail_kv_sdk_runtime::SourceProtection::File {
                path: config.path(input),
            }
        } else {
            let storage = config
                .section::<StorageConfig>("storage")?
                .resolve(&config.base_dir)
                .map_err(Error::config)?;
            actrail_kv_sdk_runtime::SourceProtection::storage(&storage)?
        };
        config.finish()?;
        Ok(Self {
            config,
            settings,
            source,
        })
    }
    fn call(&mut self, runtime: &Runtime, input: &[u8], _options: &[u8]) -> Result<Value> {
        let request = actrail_kv_sdk_runtime::request(input)?;
        let (selection, filter) =
            selection::filters(&self.settings.selection, request.get("selection"))?;
        let result = if let Some(input) = &self.settings.input {
            let file = File::open(self.config.path(input)).map_err(Error::storage)?;
            analyze_reader_with_selection(
                BufReader::new(file),
                self.settings.options.clone(),
                &selection,
            )
            .map_err(Error::operation)?
        } else {
            let storage = self
                .config
                .section::<StorageConfig>("storage")?
                .resolve(&self.config.base_dir)
                .map_err(Error::config)?;
            runtime.block_on(async {
                let store = open_store(storage, StoreAccess::ReadOnly)
                    .await
                    .map_err(Error::storage)?;
                let stream = store.scan(filter).await.map_err(Error::storage)?;
                analyze_capture_stream_with_selection(
                    stream,
                    self.settings.options.clone(),
                    &selection,
                )
                .await
                .map_err(Error::operation)
            })?
        };
        serde_json::to_value(result).map_err(Error::internal)
    }
    fn source(&self) -> Option<Value> {
        Some(self.source.json())
    }
    fn export(input: &[u8], destination: &[u8]) -> Result<Value> {
        let data = actrail_kv_sdk_runtime::analysis_data(input)?;
        let bytes = serde_json::to_vec_pretty(&data).map_err(Error::internal)?;
        actrail_kv_sdk_runtime::publish(
            destination,
            &bytes,
            actrail_kv_sdk_runtime::SourceProtection::from_response(input)?.as_ref(),
        )
    }
}
actrail_kv_sdk_runtime::export_lifecycle!(
    Analyzer,
    actrail_kv_analyzer_abi_major,
    actrail_kv_analyzer_abi_minor,
    actrail_kv_analyzer_create,
    actrail_kv_analyzer_destroy,
    actrail_kv_analyzer_buffer_free
);
/// # Safety
/// 使用本库有效句柄和可读写缓冲区，调用不得与销毁并发。
#[no_mangle]
pub unsafe extern "C" fn actrail_kv_analyzer_analyze(
    handle: Handle,
    input: InputBuffer,
    output: *mut OutputBuffer,
) -> i32 {
    unsafe { Boundary::<Analyzer>::call(handle, input, InputBuffer::new(&[]), output) }
}
/// # Safety
/// 输入为有效的完整成功响应，目标和输出缓冲区在调用期间有效。
#[no_mangle]
pub unsafe extern "C" fn actrail_kv_analyzer_export(
    input: InputBuffer,
    destination: InputBuffer,
    output: *mut OutputBuffer,
) -> i32 {
    unsafe { Boundary::<Analyzer>::export(input, destination, output) }
}
