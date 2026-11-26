//! Enforces the total managed-segment size while the caller holds the exclusive write boundary.
use super::segments::list_segments;
use crate::{NdjsonConfig, StorageError};
use std::fs;

pub(super) struct Capacity;

impl Capacity {
    pub(super) fn reserve(config: &NdjsonConfig, incoming: u64) -> Result<(), StorageError> {
        let limit = config.max_size_bytes().get();
        let target = limit
            .checked_sub(incoming)
            .ok_or(StorageError::RecordTooLarge {
                size_bytes: incoming,
                max_bytes: limit,
            })?;
        let segments = list_segments(&config.directory)?
            .into_iter()
            .map(|segment| fs::metadata(&segment.path).map(|metadata| (segment, metadata.len())))
            .collect::<std::io::Result<Vec<_>>>()?;
        let mut total: u64 = segments.iter().map(|(_, bytes)| bytes).sum();
        for (segment, bytes) in segments {
            if total <= target {
                break;
            }
            fs::remove_file(segment.path)?;
            total -= bytes;
        }
        Ok(())
    }
}
