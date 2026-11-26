//! 将分析筛选配置转换为存储粗筛与分析精筛，并保留配置默认值。
use actrail_kv_analyzer::run::{CaptureTimeRange, ReportSelection};
use actrail_kv_sdk_runtime::{Error, Result};
use actrail_kv_storage::CaptureFilter;
use analysis_contract::settings::SelectionSettings;
use config_contract::Config;
use serde_json::Value;

pub fn filters(
    settings: &SelectionSettings,
    overrides: Option<&Value>,
) -> Result<(ReportSelection, CaptureFilter)> {
    let mut table = settings.try_to_toml_table().map_err(Error::config)?;
    // 请求仅覆盖显式提供的筛选项，默认值仍由已有配置契约管理。
    if let Some(overrides) = overrides {
        let mut overrides = overrides.clone();
        if overrides.get("last_n").is_some_and(Value::is_null) {
            table.remove("last_n");
            overrides
                .as_object_mut()
                .expect("last_n belongs to object")
                .remove("last_n");
        }
        let document =
            config_contract::toml_edit::ser::to_document(&overrides).map_err(Error::input)?;
        for (key, value) in document.iter() {
            table.insert(key, value.clone());
        }
    }
    let settings = SelectionSettings::parse_toml_table(&table)
        .map_err(Error::input)?
        .0;
    let mut ranges = Vec::new();
    for range in settings.time_ranges {
        if range.from.is_none() && range.until.is_none() {
            return Err(Error::input("time range needs a bound"));
        }
        let parsed = CaptureFilter::parse(None, range.from.as_deref(), range.until.as_deref())
            .map_err(Error::input)?;
        ranges.push(CaptureTimeRange::new(parsed.from(), parsed.until()).map_err(Error::input)?);
    }
    let session = match settings.session_keys.as_slice() {
        [session] => Some(session.clone()),
        _ => None,
    };
    let from = ranges.iter().map(CaptureTimeRange::from).min().flatten();
    let until = if ranges.iter().any(|range| range.until().is_none()) {
        None
    } else {
        ranges.iter().filter_map(CaptureTimeRange::until).max()
    };
    let filter = CaptureFilter::new(session, from, until).map_err(Error::input)?;
    Ok((
        ReportSelection::new(settings.session_keys, ranges, settings.last_n),
        filter,
    ))
}
