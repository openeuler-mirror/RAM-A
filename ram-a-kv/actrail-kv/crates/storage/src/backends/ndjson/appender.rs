//! 本文件负责在单一临界区内完成 NDJSON 整行写入和刷新。

use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use actrail_kv_artifacts::CapturedRequest;
use thiserror::Error;

#[cfg(unix)]
use fs2::FileExt;

#[derive(Debug, Error)]
pub enum AppendError {
    #[error("failed to serialize captured request: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("captured request output lock is poisoned")]
    LockPoisoned,
    #[error("captured request output is locked by another receiver: {0}")]
    AlreadyLocked(io::Error),
    #[error("captured request output is unavailable after a previous write failure")]
    Failed,
    #[error("failed to append captured request: {write}; rollback also failed: {rollback}")]
    WriteAndRollback {
        write: io::Error,
        rollback: io::Error,
    },
    #[error("failed to append captured request: {0}")]
    Io(#[from] io::Error),
}

#[derive(Clone)]
pub struct NdjsonAppender {
    state: Arc<Mutex<AppendState>>,
    path: Option<PathBuf>,
}

struct AppendState {
    output: Box<dyn AppendFile>,
    failed: bool,
}

trait AppendFile: Write + Send {
    fn len(&self) -> io::Result<u64>;
    fn truncate(&mut self, length: u64) -> io::Result<()>;
}

impl AppendFile for File {
    fn len(&self) -> io::Result<u64> {
        Ok(self.metadata()?.len())
    }

    fn truncate(&mut self, length: u64) -> io::Result<()> {
        self.set_len(length)
    }
}

impl NdjsonAppender {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, AppendError> {
        let path = path.as_ref();
        let output = open_private_append_file(path).map_err(classify_open_error)?;
        try_lock_output_exclusive(&output).map_err(AppendError::AlreadyLocked)?;
        let mut appender = Self::from_output(Box::new(output));
        appender.path = Some(path.to_owned());
        Ok(appender)
    }

    pub fn append(&self, record: &CapturedRequest) -> Result<(), AppendError> {
        self.append_line(&encode_line(record)?)
    }

    pub(super) fn append_line(&self, line: &[u8]) -> Result<(), AppendError> {
        let mut state = self.state.lock().map_err(|_| AppendError::LockPoisoned)?;
        if state.failed {
            return Err(AppendError::Failed);
        }
        let original_length = match state.output.len() {
            Ok(length) => length,
            Err(error) => {
                state.failed = true;
                return Err(AppendError::Io(error));
            }
        };
        let result = state
            .output
            .write_all(line)
            .and_then(|()| state.output.flush());
        if let Err(write) = result {
            let rollback = state.output.truncate(original_length);
            state.failed = true;
            return match rollback {
                Ok(()) => Err(AppendError::Io(write)),
                Err(rollback) => Err(AppendError::WriteAndRollback { write, rollback }),
            };
        }
        Ok(())
    }

    pub(super) fn snapshot_file(&self) -> Result<(File, u64), AppendError> {
        let _boundary = self.state.lock().map_err(|_| AppendError::LockPoisoned)?;
        let path = self
            .path
            .as_ref()
            .ok_or_else(|| io::Error::other("NDJSON appender has no readable path"))?;
        let mut file = File::open(path)?;
        let end = super::segments::committed_length(&mut file)?;
        Ok((file, end))
    }

    fn from_output(output: Box<dyn AppendFile>) -> Self {
        Self {
            state: Arc::new(Mutex::new(AppendState {
                output,
                failed: false,
            })),
            path: None,
        }
    }
}

pub(super) fn encode_line(record: &CapturedRequest) -> Result<Vec<u8>, serde_json::Error> {
    let mut line = serde_json::to_vec(record)?;
    line.push(b'\n');
    Ok(line)
}

#[cfg(unix)]
fn open_private_append_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut options = OpenOptions::new();
    options.create(true).append(true).mode(0o600);
    let output = options.open(path)?;
    output.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(output)
}

#[cfg(not(unix))]
fn open_private_append_file(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_SHARE_READ: u32 = 0x0000_0001;

    let mut options = OpenOptions::new();
    // Read-only share mode is the single-writer guard: while this handle is
    // open, the OS refuses any other writer (including a second receiver)
    // with ERROR_SHARING_VIOLATION, while readers such as
    // `actrail-kv-analyze` keep full read access to the live output.
    // This is enforced by Windows for the lifetime of the handle; it is not
    // an advisory lock that other code may ignore.
    options
        .create(true)
        .append(true)
        .share_mode(FILE_SHARE_READ);
    options.open(path)
}

/// Locks the output so that a second receiver fails with
/// [`AppendError::AlreadyLocked`]; readers are never blocked, mirroring the
/// advisory `flock` semantics of the Unix path.
#[cfg(unix)]
fn try_lock_output_exclusive(output: &File) -> io::Result<()> {
    FileExt::try_lock_exclusive(output)
}

#[cfg(not(unix))]
fn try_lock_output_exclusive(_output: &File) -> io::Result<()> {
    // Single-writer exclusion on Windows is already enforced for the whole
    // file by the read-only share mode of `open_private_append_file`; no
    // explicit byte-range lock is needed. An fs2 lock (0..u64::MAX) would
    // additionally fail every concurrent read with ERROR_LOCK_VIOLATION,
    // which is what made `actrail-kv-analyze` spin forever on the live output.
    Ok(())
}

#[cfg(unix)]
fn classify_open_error(error: io::Error) -> AppendError {
    AppendError::Io(error)
}

#[cfg(not(unix))]
fn classify_open_error(error: io::Error) -> AppendError {
    // Windows rejects the second receiver's open with a sharing violation
    // instead of a lock acquisition failure.
    const ERROR_SHARING_VIOLATION: i32 = 32;
    if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION) {
        return AppendError::AlreadyLocked(error);
    }
    AppendError::Io(error)
}

#[cfg(test)]
#[path = "../../../../../tests/unit/storage/backends/ndjson/appender.rs"]
mod tests;
