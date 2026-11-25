//! 本模块提供内存分析、CLI 原子输出与公共运行配置。

mod pipeline;

pub use crate::config::AnalyzeConfig;
pub use crate::model::corpus::{CaptureTimeRange, ReportSelection};
pub use pipeline::{
    analyze_capture_stream_with_selection, analyze_captures, analyze_captures_with_selection,
    analyze_file, analyze_file_with_selection, analyze_reader, analyze_reader_with_selection,
};
