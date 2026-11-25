//! 存储配置的默认参数。
use std::num::{NonZeroU32, NonZeroU64};
pub(super) const DEFAULT_MAX_AGE_SECONDS: u32 = 7 * 24 * 60 * 60;

pub(super) fn default_max_size_bytes() -> NonZeroU64 {
    NonZeroU64::new(256 * 1024 * 1024).unwrap()
}

pub(super) fn default_segment_seconds() -> NonZeroU32 {
    NonZeroU32::new(3600).unwrap()
}

pub(super) fn default_segment_bytes() -> NonZeroU64 {
    NonZeroU64::new(64 * 1024 * 1024).unwrap()
}

pub(super) fn default_cleanup_seconds() -> NonZeroU32 {
    NonZeroU32::new(60).unwrap()
}

pub(super) fn default_delete_batch_size() -> NonZeroU32 {
    NonZeroU32::new(256).unwrap()
}
