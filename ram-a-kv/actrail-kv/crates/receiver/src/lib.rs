//! 本 crate 提供通道无关的持久采集服务与 HTTP 适配入口。

mod channels;
pub mod config;
pub mod contracts;
mod service;

pub use actrail_kv_storage::{AppendError, NdjsonAppender};
pub use channels::http::{router, router_with_limit, router_with_storage};
pub use config::ReceiverConfig;
