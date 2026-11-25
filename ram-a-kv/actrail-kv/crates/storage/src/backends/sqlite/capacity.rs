//! Tracks retained JSON bytes and evicts the oldest rows within the caller's transaction.
use crate::{SqliteConfig, StorageError};
use rusqlite::{params, Connection};

pub(super) struct Capacity {
    limit: u64,
    batch_size: u32,
}

impl Capacity {
    pub(super) fn new(config: &SqliteConfig) -> Self {
        Self {
            limit: config.max_size_bytes().get(),
            batch_size: config.delete_batch_size().get(),
        }
    }

    pub(super) fn initialize(connection: &Connection) -> rusqlite::Result<()> {
        // Backfill existing databases once; triggers also account for time-based deletions.
        connection.execute_batch("
            CREATE TABLE IF NOT EXISTS capture_usage (
                id INTEGER PRIMARY KEY,
                total_bytes INTEGER NOT NULL
            );
            INSERT INTO capture_usage (id, total_bytes)
            SELECT 1, COALESCE((SELECT SUM(length(CAST(record AS BLOB))) FROM captures), 0)
            WHERE NOT EXISTS (SELECT 1 FROM capture_usage WHERE id = 1);
            CREATE TRIGGER IF NOT EXISTS captures_usage_insert AFTER INSERT ON captures BEGIN
                UPDATE capture_usage SET total_bytes = total_bytes + length(CAST(NEW.record AS BLOB)) WHERE id = 1;
            END;
            CREATE TRIGGER IF NOT EXISTS captures_usage_delete AFTER DELETE ON captures BEGIN
                UPDATE capture_usage SET total_bytes = total_bytes - length(CAST(OLD.record AS BLOB)) WHERE id = 1;
            END;
            CREATE TRIGGER IF NOT EXISTS captures_usage_update AFTER UPDATE OF record ON captures BEGIN
                UPDATE capture_usage SET total_bytes = total_bytes + length(CAST(NEW.record AS BLOB)) - length(CAST(OLD.record AS BLOB)) WHERE id = 1;
            END;
        ")
    }

    pub(super) fn reserve(
        &self,
        connection: &Connection,
        incoming: u64,
    ) -> Result<(), StorageError> {
        let target = self
            .limit
            .checked_sub(incoming)
            .ok_or(StorageError::RecordTooLarge {
                size_bytes: incoming,
                max_bytes: self.limit,
            })?;
        let mut total: u64 = connection.query_row(
            "SELECT total_bytes FROM capture_usage WHERE id = 1",
            [],
            |row| row.get::<_, i64>(0).map(|bytes| bytes as u64),
        )?;
        while total > target {
            let rows = connection.prepare(
                "SELECT id, length(CAST(record AS BLOB)) FROM captures ORDER BY captured_at, id LIMIT ?",
            )?.query_map([self.batch_size], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)? as u64)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (id, bytes) in rows {
                connection.execute("DELETE FROM captures WHERE id = ?", params![id])?;
                total -= bytes;
                if total <= target {
                    break;
                }
            }
        }
        Ok(())
    }
}
