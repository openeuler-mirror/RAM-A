//! 为可独立升级的原生动态库提供安全的 Rust 采集、分析和报告接口。
pub mod analyzer;
pub mod contracts;
mod loader;
pub mod receiver;
pub mod reporter;
pub use analyzer::{AnalysisOutput, Analyzer, AnalyzerFactory, RecordLimit, Selection, TimeRange};
pub use contracts::*;
pub use receiver::{CaptureComparison, CaptureInput, CaptureReceipt, Receiver, ReceiverFactory};
pub use reporter::{RenderOptions, ReportFormat, ReportOutput, Reporter, ReporterFactory};
