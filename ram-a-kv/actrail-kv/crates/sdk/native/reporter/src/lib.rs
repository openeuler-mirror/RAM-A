//! 直接消费分析响应并渲染内存报告，语言设置限定于当前实例。
use actrail_kv_reporter::{
    read_result, render_html_with_locale, render_markdown_with_locale, Locale, LocaleFactory,
    ReportFormat,
};
use actrail_kv_sdk_runtime::{abi::*, Application, Boundary, Config, Error, Result, Runtime};
use report_contract::ReportSettings;
use serde_json::{json, Value};

struct Reporter {
    settings: ReportSettings,
    source: Option<actrail_kv_sdk_runtime::SourceProtection>,
}
impl Application for Reporter {
    fn create(mut config: Config, _runtime: &Runtime) -> Result<Self> {
        let settings = config.section("report")?;
        config.finish()?;
        Ok(Self {
            settings,
            source: None,
        })
    }
    fn call(&mut self, _runtime: &Runtime, input: &[u8], options: &[u8]) -> Result<Value> {
        let options = actrail_kv_sdk_runtime::request(options)?;
        let data = actrail_kv_sdk_runtime::analysis_data(input)?;
        self.source = actrail_kv_sdk_runtime::SourceProtection::from_response(input)?;
        let bytes = serde_json::to_vec(&data).map_err(Error::internal)?;
        let result = read_result(bytes.as_slice()).map_err(Error::operation)?;
        let locale = options
            .get("locale")
            .and_then(Value::as_str)
            .unwrap_or(&self.settings.locale);
        let locale =
            Locale::parse(locale).ok_or_else(|| Error::input("unsupported report locale"))?;
        let format = options
            .get("format")
            .map(|value| {
                serde_json::from_value::<ReportFormat>(value.clone()).map_err(Error::input)
            })
            .transpose()?
            .or(self.settings.format);
        // 单个输出对应一个格式，缺省双格式沿用原有配置含义。
        let formats = match format {
            Some(format) => vec![format],
            None => vec![ReportFormat::Html, ReportFormat::Markdown],
        };
        let mut reports = Vec::new();
        for format in formats {
            let locale = LocaleFactory::snapshot_for(locale).map_err(Error::operation)?;
            let content = match format {
                ReportFormat::Html => render_html_with_locale(&result, locale),
                ReportFormat::Markdown => render_markdown_with_locale(&result, locale),
            }
            .map_err(Error::operation)?;
            reports.push(json!({"format": format, "content": content}));
        }
        Ok(json!({"reports": reports}))
    }
    fn source(&self) -> Option<Value> {
        self.source.as_ref().map(|source| source.json())
    }
    fn export(input: &[u8], destination: &[u8]) -> Result<Value> {
        let data = actrail_kv_sdk_runtime::analysis_data(input)?;
        let destination_value = actrail_kv_sdk_runtime::request(destination)?;
        let requested = destination_value.get("format").and_then(Value::as_str);
        let reports = data
            .get("reports")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::input("missing reports"))?;
        let report = if let Some(format) = requested {
            reports
                .iter()
                .find(|report| report.get("format").and_then(Value::as_str) == Some(format))
        } else if reports.len() == 1 {
            reports.first()
        } else {
            None
        }
        .ok_or_else(|| {
            Error::input("select an available format when exporting multiple reports")
        })?;
        let content = report
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::input("missing report content"))?;
        actrail_kv_sdk_runtime::publish(
            destination,
            content.as_bytes(),
            actrail_kv_sdk_runtime::SourceProtection::from_response(input)?.as_ref(),
        )
    }
}
actrail_kv_sdk_runtime::export_lifecycle!(
    Reporter,
    actrail_kv_reporter_abi_major,
    actrail_kv_reporter_abi_minor,
    actrail_kv_reporter_create,
    actrail_kv_reporter_destroy,
    actrail_kv_reporter_buffer_free
);
/// # Safety
/// 分析和参数缓冲区仅借用，调用者保持它们有效直到返回。
#[no_mangle]
pub unsafe extern "C" fn actrail_kv_reporter_render(
    handle: Handle,
    input: InputBuffer,
    options: InputBuffer,
    output: *mut OutputBuffer,
) -> i32 {
    unsafe { Boundary::<Reporter>::call(handle, input, options, output) }
}
/// # Safety
/// 报告响应、导出参数及输出地址必须在调用期间有效。
#[no_mangle]
pub unsafe extern "C" fn actrail_kv_reporter_export(
    input: InputBuffer,
    destination: InputBuffer,
    output: *mut OutputBuffer,
) -> i32 {
    unsafe { Boundary::<Reporter>::export(input, destination, output) }
}
