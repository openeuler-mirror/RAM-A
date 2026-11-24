//! SQLite 后端负责事务写入、索引筛选、快照扫描和按批清理。

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use actrail_kv_artifacts::CapturedRequest;
use futures_util::future::BoxFuture;
use rusqlite::{params, params_from_iter, Connection, OpenFlags};

use super::StoreAccess;
use crate::{
    filter::{capture_time, time_key},
    runtime::{scan_blocking, ScanWriter},
    CaptureFilter, CaptureStore, CaptureStream, RetentionConfig, SqliteConfig, StorageError,
};

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS captures (
        id INTEGER PRIMARY KEY,
        session_key TEXT,
        captured_at TEXT NOT NULL,
        record TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS captures_session_time ON captures (session_key, captured_at);
    CREATE INDEX IF NOT EXISTS captures_time ON captures (captured_at);
";

pub(super) struct SqliteStore {
    path: PathBuf,
    writer: Option<Arc<Mutex<Connection>>>,
    retention: Option<RetentionConfig>,
    delete_batch_size: u32,
}

impl SqliteStore {
    pub(super) fn open(config: SqliteConfig, access: StoreAccess) -> Result<Self, StorageError> {
        if config.path.as_os_str() == ":memory:" {
            return Err(StorageError::InvalidConfig(
                "SQLite requires a database file".into(),
            ));
        }
        let writer = match access {
            StoreAccess::ReadWrite => {
                match fs::metadata(&config.path) {
                    Ok(metadata) if !metadata.is_file() => {
                        return Err(StorageError::InvalidConfig(
                            "SQLite requires a regular database file".into(),
                        ));
                    }
                    Ok(_) => (),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                    Err(error) => return Err(error.into()),
                }
                let mut connection = Connection::open_with_flags(
                    &config.path,
                    OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
                )?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&config.path, fs::Permissions::from_mode(0o600))?;
                }
                let mode: String =
                    connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
                if mode != "wal" {
                    return Err(StorageError::InvalidConfig(
                        "could not enable SQLite WAL mode".into(),
                    ));
                }
                let schema = connection.transaction()?;
                schema.execute_batch(SCHEMA)?;
                schema.commit()?;
                Some(Arc::new(Mutex::new(connection)))
            }
            StoreAccess::ReadOnly => {
                open_reader(&config.path)?.prepare("SELECT record FROM captures LIMIT 0")?;
                None
            }
        };
        let store = Self {
            path: config.path,
            writer,
            retention: config.retention,
            delete_batch_size: config.delete_batch_size.get(),
        };
        if access == StoreAccess::ReadWrite {
            store.prune()?;
        }
        Ok(store)
    }

    pub(super) fn prune(&self) -> Result<(), StorageError> {
        let Some(config) = self.retention else {
            return Ok(());
        };
        let writer = self.writer.as_ref().ok_or(StorageError::ReadOnly)?;
        let cutoff = time_key(config.cutoff())?;
        loop {
            let count = lock_writer(writer)?.execute(
                "DELETE FROM captures WHERE id IN (
                    SELECT id FROM captures WHERE captured_at < ? ORDER BY captured_at LIMIT ?
                )",
                params![cutoff, self.delete_batch_size],
            )?;
            if count < self.delete_batch_size as usize {
                return Ok(());
            }
        }
    }
}

impl CaptureStore for SqliteStore {
    fn append(&self, record: CapturedRequest) -> BoxFuture<'_, Result<(), StorageError>> {
        Box::pin(async move {
            let writer = self.writer.as_ref().ok_or(StorageError::ReadOnly)?.clone();
            let retention = self.retention;
            tokio::task::spawn_blocking(move || {
                let captured_at = capture_time(&record.captured_at)?;
                let record_json = serde_json::to_string(&record)?;
                let writer = lock_writer(&writer)?;
                if let Some(retention) = retention {
                    retention.check(captured_at)?;
                }
                writer.execute(
                    "INSERT INTO captures (session_key, captured_at, record) VALUES (?, ?, ?)",
                    params![record.session_key, time_key(captured_at)?, record_json],
                )?;
                Ok(())
            })
            .await?
        })
    }

    fn scan(&self, filter: CaptureFilter) -> BoxFuture<'_, Result<CaptureStream, StorageError>> {
        let path = self.path.clone();
        let retention = self.retention;
        Box::pin(scan_blocking(move |writer| {
            scan_rows(&path, filter, retention, writer)
        }))
    }
}

fn open_reader(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
}

fn scan_rows(
    path: &Path,
    filter: CaptureFilter,
    retention: Option<RetentionConfig>,
    mut writer: ScanWriter,
) -> Result<(), StorageError> {
    let mut connection = open_reader(path)?;
    let transaction = connection.transaction()?;
    let mut sql = String::from("SELECT record FROM captures WHERE 1=1");
    let mut parameters = Vec::new();
    if let Some(session) = filter.session_key() {
        sql.push_str(" AND session_key = ?");
        parameters.push(session.to_owned());
    }
    if let Some(start) = filter.start(retention.map(RetentionConfig::cutoff)) {
        sql.push_str(" AND captured_at >= ?");
        parameters.push(time_key(start)?);
    }
    if let Some(end) = filter.until() {
        sql.push_str(" AND captured_at < ?");
        parameters.push(time_key(end)?);
    }
    sql.push_str(" ORDER BY captured_at, id");
    let mut statement = transaction.prepare(&sql)?;
    let mut rows = statement.query(params_from_iter(parameters))?;
    // SQLite fixes the read snapshot at the first step, before handing it to callers.
    let mut row = rows.next()?;
    if writer.start() {
        while let Some(current) = row {
            let json: rusqlite::types::ValueRef<'_> = current.get_ref(0)?;
            let json = json.as_str().map_err(|error| StorageError::Backend {
                backend: "SQLite",
                source: Box::new(error),
            })?;
            if !writer.send(serde_json::from_str(json)?) {
                break;
            }
            row = rows.next()?;
        }
    }
    Ok(())
}

fn lock_writer(writer: &Mutex<Connection>) -> Result<MutexGuard<'_, Connection>, StorageError> {
    writer
        .lock()
        .map_err(|_| std::io::Error::other("SQLite writer lock is poisoned").into())
}

impl From<rusqlite::Error> for StorageError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Backend {
            backend: "SQLite",
            source: Box::new(error),
        }
    }
}
