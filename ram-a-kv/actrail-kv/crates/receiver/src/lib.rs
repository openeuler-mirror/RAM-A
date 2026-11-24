//! 本 crate 提供 HTTP 接收器并将完整请求提交给采集存储。

pub mod config;
mod http_receiver;

pub use actrail_kv_storage::{AppendError, NdjsonAppender};
pub use config::ReceiverConfig;
pub use http_receiver::{router, router_with_limit, router_with_storage};
