//! 本文件解析报告输入筛选参数并生成可由存储执行的粗筛条件。

use std::num::NonZeroUsize;

use actrail_kv_analyzer::run::{CaptureTimeRange, ReportSelection};
use actrail_kv_storage::CaptureFilter;
use anyhow::Result;
use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct SelectionArgs {
    /// Select a session key. Repeat to select multiple sessions.
    #[arg(long)]
    session_key: Vec<String>,
    /// Inclusive lower bound for a single time range.
    #[arg(long)]
    from: Option<String>,
    /// Exclusive upper bound for a single time range.
    #[arg(long)]
    until: Option<String>,
    /// A time range. Repeat to select the union of multiple ranges.
    #[arg(long, value_name = "FROM..UNTIL", value_parser = parse_range, conflicts_with_all = ["from", "until"])]
    time_range: Vec<CaptureTimeRange>,
    /// Keep the latest N matching captures by capture time.
    #[arg(long)]
    last_n: Option<NonZeroUsize>,
}

impl SelectionArgs {
    pub(crate) fn into_filters(self) -> Result<(ReportSelection, CaptureFilter)> {
        let mut ranges = self.time_range;
        if self.from.is_some() || self.until.is_some() {
            ranges.push(bounds(self.from.as_deref(), self.until.as_deref())?);
        }
        let session = match self.session_key.as_slice() {
            [session] => Some(session.clone()),
            _ => None,
        };
        // Limit the scan to an indexed envelope when possible. Analysis applies each exact range.
        let from = ranges.iter().map(CaptureTimeRange::from).min().flatten();
        let until = if ranges.iter().any(|range| range.until().is_none()) {
            None
        } else {
            ranges.iter().filter_map(CaptureTimeRange::until).max()
        };
        Ok((
            ReportSelection::new(self.session_key, ranges, self.last_n),
            CaptureFilter::new(session, from, until)?,
        ))
    }
}

fn parse_range(value: &str) -> Result<CaptureTimeRange, String> {
    let (from, until) = value
        .split_once("..")
        .filter(|(from, until)| !from.is_empty() && !until.is_empty())
        .ok_or_else(|| "expected a time range in the form FROM..UNTIL".to_owned())?;
    bounds(Some(from), Some(until)).map_err(|error| error.to_string())
}

fn bounds(from: Option<&str>, until: Option<&str>) -> Result<CaptureTimeRange> {
    let parsed = CaptureFilter::parse(None, from, until)?;
    CaptureTimeRange::new(parsed.from(), parsed.until())
}
