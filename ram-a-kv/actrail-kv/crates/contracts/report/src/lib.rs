//! 提供独立于报告渲染实现的配置与输出格式契约。
mod format;
pub mod settings;

pub use format::ReportFormat;
pub use settings::ReportSettings;
