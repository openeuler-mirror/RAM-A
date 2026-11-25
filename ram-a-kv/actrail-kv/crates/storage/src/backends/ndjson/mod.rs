//! 托管 NDJSON 后端隐藏分片轮转、扫描边界和保留期回收。

mod appender;
mod capacity;
mod segments;
use capacity::Capacity;

use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use actrail_kv_artifacts::CapturedRequest;
use fs2::FileExt;
use futures_util::future::BoxFuture;

use super::{open_private_file, StoreAccess};
use crate::{
    filter::capture_time,
    runtime::{scan_blocking, ScanWriter},
    CaptureFilter, CaptureStore, CaptureStream, NdjsonConfig, RetentionConfig, StorageError,
};

pub use appender::{AppendError, NdjsonAppender};
use segments::{committed_length, list_segments, repair_tail, scan_segment, Segment, SegmentKey};

struct Writer {
    _lease: File,
    current: Mutex<Option<Segment>>,
}

pub(super) struct NdjsonStore {
    config: NdjsonConfig,
    writer: Option<Arc<Writer>>,
}

impl NdjsonStore {
    pub(super) fn open(config: NdjsonConfig, access: StoreAccess) -> Result<Self, StorageError> {
        let writer = match access {
            StoreAccess::ReadWrite => {
                fs::create_dir_all(&config.directory)?;
                let lease = open_private_file(&config.directory.join(".writer.lock"))?;
                FileExt::try_lock_exclusive(&lease)?;
                open_private_file(&snapshot_lock_path(&config.directory))?;
                Some(Arc::new(Writer {
                    _lease: lease,
                    current: Mutex::new(None),
                }))
            }
            StoreAccess::ReadOnly => {
                File::open(snapshot_lock_path(&config.directory))?;
                None
            }
        };
        let store = Self { config, writer };
        if access == StoreAccess::ReadWrite {
            store.prune()?;
        }
        Ok(store)
    }

    pub(super) fn prune(&self) -> Result<(), StorageError> {
        let _boundary = exclusive_boundary(&self.config.directory)?;
        if let Some(retention) = self.config.retention_policy() {
            let cutoff = retention.cutoff().unix_timestamp();
            let max_segments = self
                .config
                .retention
                .as_ref()
                .and_then(|settings| settings.max_segments_per_cleanup)
                .map_or(usize::MAX, |limit| limit.get() as usize);
            for segment in list_segments(&self.config.directory)?
                .into_iter()
                .filter(|segment| segment.key.end <= cutoff)
                .take(max_segments)
            {
                fs::remove_file(segment.path)?;
            }
        }
        Capacity::reserve(&self.config, 0)?;
        Ok(())
    }
}

impl Writer {
    fn append(&self, config: &NdjsonConfig, record: CapturedRequest) -> Result<(), StorageError> {
        let captured_at = capture_time(&record.captured_at)?;
        let key = SegmentKey::for_timestamp(
            captured_at.unix_timestamp(),
            config.segment_seconds.get().into(),
        );
        let line = appender::encode_line(&record)?;

        let mut current = self
            .current
            .lock()
            .map_err(|_| io::Error::other("NDJSON writer lock is poisoned"))?;
        let _boundary = exclusive_boundary(&config.directory)?;
        if let Some(retention) = config.retention_policy() {
            retention.check(captured_at)?;
        }
        Capacity::reserve(config, line.len() as u64)?;
        let previous = match current.as_ref() {
            Some(segment) if segment.key.in_window(key) => Some(segment.clone()),
            _ => list_segments(&config.directory)?
                .into_iter()
                .rev()
                .find(|segment| segment.key.in_window(key)),
        };
        let segment = match previous {
            Some(segment)
                if repair_tail(&segment.path)?.is_some_and(|size| {
                    size == 0
                        || size.saturating_add(line.len() as u64) <= config.max_segment_bytes.get()
                }) =>
            {
                segment
            }
            Some(segment) => Segment::create(&config.directory, segment.key.next()?)?,
            None => Segment::create(&config.directory, key)?,
        };
        let path = segment.path.clone();
        *current = Some(segment);
        NdjsonAppender::open(&path)?.append_line(&line)?;
        Ok(())
    }
}

impl CaptureStore for NdjsonStore {
    fn append(&self, record: CapturedRequest) -> BoxFuture<'_, Result<(), StorageError>> {
        Box::pin(async move {
            let writer = self.writer.as_ref().ok_or(StorageError::ReadOnly)?.clone();
            let config = self.config.clone();
            tokio::task::spawn_blocking(move || writer.append(&config, record)).await?
        })
    }

    fn scan(&self, filter: CaptureFilter) -> BoxFuture<'_, Result<CaptureStream, StorageError>> {
        let config = self.config.clone();
        Box::pin(scan_blocking(move |writer| {
            scan_store(&config, filter, writer)
        }))
    }
}

fn scan_store(
    config: &NdjsonConfig,
    filter: CaptureFilter,
    mut writer: ScanWriter,
) -> Result<(), StorageError> {
    let segments = {
        let boundary = File::open(snapshot_lock_path(&config.directory))?;
        FileExt::lock_shared(&boundary)?;
        let start = filter.start(config.retention_policy().map(RetentionConfig::cutoff));
        let mut segments = Vec::new();
        for segment in list_segments(&config.directory)? {
            if writer.is_closed() {
                return Ok(());
            }
            if segment.key.overlaps(start, filter.until()) {
                let path = segment.path;
                let length = committed_length(&mut File::open(&path)?)?;
                if length > 0 {
                    segments.push((path, length));
                }
            }
        }
        (start, segments)
    };
    let (start, segments) = segments;
    if writer.start() {
        for (path, length) in segments {
            if writer.is_closed() {
                break;
            }
            let file = File::open(path).map_err(|error| {
                if error.kind() == io::ErrorKind::NotFound {
                    StorageError::SnapshotExpired
                } else {
                    error.into()
                }
            })?;
            scan_segment(file, length, &filter, start, &writer)?;
        }
    }
    Ok(())
}

fn snapshot_lock_path(directory: &Path) -> PathBuf {
    directory.join(".snapshot.lock")
}

fn exclusive_boundary(directory: &Path) -> io::Result<File> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(snapshot_lock_path(directory))?;
    FileExt::lock_exclusive(&file)?;
    Ok(file)
}

impl CaptureStore for NdjsonAppender {
    fn append(&self, record: CapturedRequest) -> BoxFuture<'_, Result<(), StorageError>> {
        let appender = self.clone();
        Box::pin(async move {
            Ok(tokio::task::spawn_blocking(move || appender.append(&record)).await??)
        })
    }

    fn scan(&self, filter: CaptureFilter) -> BoxFuture<'_, Result<CaptureStream, StorageError>> {
        let appender = self.clone();
        Box::pin(scan_blocking(move |mut writer| {
            let (file, end) = appender.snapshot_file()?;
            if writer.start() {
                scan_segment(file, end, &filter, filter.from(), &writer)?;
            }
            Ok(())
        }))
    }
}

impl From<AppendError> for StorageError {
    fn from(error: AppendError) -> Self {
        Self::Backend {
            backend: "NDJSON",
            source: Box::new(error),
        }
    }
}
