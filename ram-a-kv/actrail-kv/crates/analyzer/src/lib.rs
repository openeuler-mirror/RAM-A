//! 本文件公开离线分析器的模型、模板发现、诊断、排序与运行流水线。

mod cache_metrics;
pub mod config;
pub mod diagnosis;
pub mod discovery;
pub mod model;
mod prefix_view;
pub mod ranking;
pub mod run;
mod session;
