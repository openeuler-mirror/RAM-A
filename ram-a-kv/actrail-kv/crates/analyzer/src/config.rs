//! 导出共享分析配置，并组织分析命令自身的输入输出设置。
pub use analysis_contract::{config::*, settings};

#[cfg(test)]
#[path = "../../../tests/unit/analyzer/config.rs"]
mod tests;
