//! 本文件管理 NDJSON 分片标识及有界的整行读取。

use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use actrail_kv_artifacts::CapturedRequest;
use time::OffsetDateTime;

use crate::{filter::capture_time, runtime::ScanWriter, CaptureFilter, StorageError};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct SegmentKey {
    start: i64,
    pub(super) end: i64,
    index: u64,
}

impl SegmentKey {
    pub(super) fn for_timestamp(timestamp: i64, period: i64) -> Self {
        let start = timestamp.div_euclid(period) * period;
        Self {
            start,
            end: start + period,
            index: 0,
        }
    }

    pub(super) fn in_window(self, other: Self) -> bool {
        self.start == other.start && self.end == other.end
    }

    pub(super) fn overlaps(
        self,
        from: Option<OffsetDateTime>,
        until: Option<OffsetDateTime>,
    ) -> bool {
        from.is_none_or(|from| from.unix_timestamp() < self.end)
            && until.is_none_or(|until| {
                i128::from(self.start) * 1_000_000_000 < until.unix_timestamp_nanos()
            })
    }

    pub(super) fn next(self) -> io::Result<Self> {
        let index = self
            .index
            .checked_add(1)
            .ok_or_else(|| io::Error::other("NDJSON segment sequence exhausted"))?;
        Ok(Self { index, ..self })
    }

    fn prefix(self) -> String {
        format!("capture_{}_{}_{}_", self.start, self.end, self.index)
    }

    fn parse(name: &str) -> Option<Self> {
        let suffix = name.strip_prefix("capture_")?.strip_suffix(".ndjson")?;
        let mut fields = suffix.split('_');
        let segment = Self {
            start: fields.next()?.parse().ok()?,
            end: fields.next()?.parse().ok()?,
            index: fields.next()?.parse().ok()?,
        };
        let identity = fields.next()?;
        (segment.start < segment.end
            && identity.len() == 12
            && identity.bytes().all(|byte| byte.is_ascii_alphanumeric())
            && fields.next().is_none())
        .then_some(segment)
    }
}

#[derive(Clone)]
pub(super) struct Segment {
    pub(super) key: SegmentKey,
    pub(super) path: PathBuf,
}

impl Segment {
    pub(super) fn create(directory: &Path, key: SegmentKey) -> io::Result<Self> {
        let temporary = tempfile::Builder::new()
            .prefix(&key.prefix())
            .rand_bytes(12)
            .suffix(".ndjson")
            .tempfile_in(directory)?;
        let (_, path) = temporary.keep().map_err(|error| error.error)?;
        Ok(Self { key, path })
    }
}

pub(super) fn list_segments(directory: &Path) -> io::Result<Vec<Segment>> {
    let mut segments = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            if let Some(key) = entry.file_name().to_str().and_then(SegmentKey::parse) {
                segments.push(Segment {
                    key,
                    path: entry.path(),
                });
            }
        }
    }
    segments
        .sort_unstable_by(|left, right| left.key.cmp(&right.key).then(left.path.cmp(&right.path)));
    Ok(segments)
}

pub(super) fn repair_tail(path: &Path) -> io::Result<Option<u64>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => (),
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected a regular NDJSON segment",
            ))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let committed = committed_length(&mut file)?;
    if file.metadata()?.len() > committed {
        file.set_len(committed)?;
    }
    Ok(Some(committed))
}

pub(super) fn committed_length(file: &mut File) -> io::Result<u64> {
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(0);
    }
    let mut buffer = [0; 4096];
    file.seek(SeekFrom::Start(len - 1))?;
    file.read_exact(&mut buffer[..1])?;
    if buffer[0] == b'\n' {
        return Ok(len);
    }
    let mut end = len - 1;
    while end > 0 {
        let start = end.saturating_sub(buffer.len() as u64);
        let count = (end - start) as usize;
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buffer[..count])?;
        if let Some(newline) = buffer[..count].iter().rposition(|&byte| byte == b'\n') {
            return Ok(start + newline as u64 + 1);
        }
        end = start;
    }
    Ok(0)
}

pub(super) fn scan_segment(
    mut file: File,
    length: u64,
    filter: &CaptureFilter,
    from: Option<OffsetDateTime>,
    writer: &ScanWriter,
) -> Result<(), StorageError> {
    file.rewind()?;
    let mut reader = BufReader::new(file.take(length));
    let mut line = Vec::new();
    while !writer.is_closed() {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            if reader.get_ref().limit() != 0 {
                return Err(StorageError::SnapshotExpired);
            }
            break;
        }
        if line.last() != Some(&b'\n') {
            return Err(StorageError::SnapshotExpired);
        }
        let record: CapturedRequest = serde_json::from_slice(&line)?;
        if filter
            .session_key()
            .is_some_and(|session| record.session_key.as_deref() != Some(session))
        {
            continue;
        }
        let captured_at = capture_time(&record.captured_at)?;
        if from.is_some_and(|from| captured_at < from)
            || filter.until().is_some_and(|until| captured_at >= until)
        {
            continue;
        }
        if !writer.send(record) {
            break;
        }
    }
    Ok(())
}
