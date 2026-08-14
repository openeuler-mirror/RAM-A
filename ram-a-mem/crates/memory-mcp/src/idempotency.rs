use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdempotencyEntry {
    pub scope_id: String,
    pub conversation_id: String,
    pub message_id: String,
    pub content_hash: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Reservation {
    Cached {
        results: Vec<Value>,
    },
    Proceed {
        pipeline_run_id: String,
        candidate_message_ids: Vec<String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdempotencyError {
    Conflict,
    Storage,
}

impl fmt::Display for IdempotencyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Conflict => "idempotency key conflicts with an earlier request",
            Self::Storage => "idempotency storage operation failed",
        })
    }
}

impl std::error::Error for IdempotencyError {}

#[derive(Clone, Debug)]
pub struct IdempotencyRepository {
    path: PathBuf,
}

impl IdempotencyRepository {
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self, IdempotencyError> {
        let repository = Self { path: path.into() };
        let path = repository.path.clone();
        run_sqlite(move || {
            open_connection(&path)?;
            Ok(())
        })
        .await?;
        Ok(repository)
    }

    pub async fn reserve(
        &self,
        entries: &[IdempotencyEntry],
        pipeline_run_id: &str,
    ) -> Result<Reservation, IdempotencyError> {
        let path = self.path.clone();
        let entries = entries.to_vec();
        let pipeline_run_id = pipeline_run_id.to_string();
        run_sqlite(move || reserve_sync(&path, &entries, &pipeline_run_id)).await
    }

    pub async fn complete(
        &self,
        entries: &[IdempotencyEntry],
        pipeline_run_id: &str,
        result: &Value,
    ) -> Result<(), IdempotencyError> {
        let path = self.path.clone();
        let entries = entries.to_vec();
        let pipeline_run_id = pipeline_run_id.to_string();
        let result = serde_json::to_string(result).map_err(|_| IdempotencyError::Storage)?;
        run_sqlite(move || complete_sync(&path, &entries, &pipeline_run_id, &result)).await
    }
}

async fn run_sqlite<T, F>(operation: F) -> Result<T, IdempotencyError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, IdempotencyError> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| IdempotencyError::Storage)?
}

fn open_connection(path: &Path) -> Result<Connection, IdempotencyError> {
    if path != Path::new(":memory:") {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|_| IdempotencyError::Storage)?;
        }
    }
    let connection = Connection::open(path).map_err(|_| IdempotencyError::Storage)?;
    connection
        .busy_timeout(Duration::from_millis(5_000))
        .map_err(|_| IdempotencyError::Storage)?;
    if path != Path::new(":memory:") {
        connection
            .query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))
            .map_err(|_| IdempotencyError::Storage)?;
    }
    connection
        .execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS mcp_ingest_idempotency (
                scope_id TEXT NOT NULL,
                conversation_id TEXT NOT NULL,
                message_id TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                status TEXT NOT NULL CHECK (status IN ('pending', 'success')),
                pipeline_run_id TEXT NOT NULL,
                result_json TEXT,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY (scope_id, conversation_id, message_id)
            );
            "#,
        )
        .map_err(|_| IdempotencyError::Storage)?;
    Ok(connection)
}

fn reserve_sync(
    path: &Path,
    entries: &[IdempotencyEntry],
    pipeline_run_id: &str,
) -> Result<Reservation, IdempotencyError> {
    let mut connection = open_connection(path)?;
    let transaction = connection
        .transaction()
        .map_err(|_| IdempotencyError::Storage)?;
    let mut cached = Vec::new();
    let mut candidate_message_ids = Vec::new();
    let now = current_time_ms();

    for entry in entries {
        let existing = transaction
            .query_row(
                "SELECT content_hash, status, result_json FROM mcp_ingest_idempotency
                 WHERE scope_id = ?1 AND conversation_id = ?2 AND message_id = ?3",
                params![entry.scope_id, entry.conversation_id, entry.message_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|_| IdempotencyError::Storage)?;

        match existing {
            Some((stored_hash, _, _)) if stored_hash != entry.content_hash => {
                return Err(IdempotencyError::Conflict);
            }
            Some((_, status, Some(result))) if status == "success" => {
                cached.push(serde_json::from_str(&result).map_err(|_| IdempotencyError::Storage)?);
            }
            Some(_) => {
                transaction
                    .execute(
                        "UPDATE mcp_ingest_idempotency
                         SET pipeline_run_id = ?4, updated_at_ms = ?5
                         WHERE scope_id = ?1 AND conversation_id = ?2 AND message_id = ?3",
                        params![
                            entry.scope_id,
                            entry.conversation_id,
                            entry.message_id,
                            pipeline_run_id,
                            now,
                        ],
                    )
                    .map_err(|_| IdempotencyError::Storage)?;
                candidate_message_ids.push(entry.message_id.clone());
            }
            None => {
                transaction
                    .execute(
                        "INSERT INTO mcp_ingest_idempotency
                         (scope_id, conversation_id, message_id, content_hash, status,
                          pipeline_run_id, result_json, created_at_ms, updated_at_ms)
                         VALUES (?1, ?2, ?3, ?4, 'pending', ?5, NULL, ?6, ?6)",
                        params![
                            entry.scope_id,
                            entry.conversation_id,
                            entry.message_id,
                            entry.content_hash,
                            pipeline_run_id,
                            now,
                        ],
                    )
                    .map_err(|_| IdempotencyError::Storage)?;
                candidate_message_ids.push(entry.message_id.clone());
            }
        }
    }

    transaction
        .commit()
        .map_err(|_| IdempotencyError::Storage)?;
    if candidate_message_ids.is_empty() {
        Ok(Reservation::Cached { results: cached })
    } else {
        Ok(Reservation::Proceed {
            pipeline_run_id: pipeline_run_id.to_string(),
            candidate_message_ids,
        })
    }
}

fn complete_sync(
    path: &Path,
    entries: &[IdempotencyEntry],
    pipeline_run_id: &str,
    result_json: &str,
) -> Result<(), IdempotencyError> {
    let mut connection = open_connection(path)?;
    let transaction = connection
        .transaction()
        .map_err(|_| IdempotencyError::Storage)?;
    let now = current_time_ms();
    for entry in entries {
        let updated = transaction
            .execute(
                "UPDATE mcp_ingest_idempotency
                 SET status = 'success', pipeline_run_id = ?5, result_json = ?6, updated_at_ms = ?7
                 WHERE scope_id = ?1 AND conversation_id = ?2 AND message_id = ?3
                   AND content_hash = ?4",
                params![
                    entry.scope_id,
                    entry.conversation_id,
                    entry.message_id,
                    entry.content_hash,
                    pipeline_run_id,
                    result_json,
                    now,
                ],
            )
            .map_err(|_| IdempotencyError::Storage)?;
        if updated != 1 {
            return Err(IdempotencyError::Storage);
        }
    }
    transaction.commit().map_err(|_| IdempotencyError::Storage)
}

fn current_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{IdempotencyEntry, IdempotencyError, IdempotencyRepository, Reservation};

    fn entry(hash: &str) -> IdempotencyEntry {
        IdempotencyEntry {
            scope_id: "scope-a".to_string(),
            conversation_id: "conversation-1".to_string(),
            message_id: "message-1".to_string(),
            content_hash: hash.to_string(),
        }
    }

    #[tokio::test]
    async fn completed_entries_are_reused_from_the_same_wal_database() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("memory.sqlite");
        let repository = IdempotencyRepository::open(&path).await.unwrap();

        let first = repository
            .reserve(&[entry("hash-a")], "run-1")
            .await
            .unwrap();
        assert!(matches!(first, Reservation::Proceed { .. }));
        repository
            .complete(
                &[entry("hash-a")],
                "run-1",
                &json!({"memory_ids": ["mem-1"]}),
            )
            .await
            .unwrap();
        let repeated = repository
            .reserve(&[entry("hash-a")], "run-2")
            .await
            .unwrap();
        assert_eq!(
            repeated,
            Reservation::Cached {
                results: vec![json!({"memory_ids": ["mem-1"]})]
            }
        );

        let connection = rusqlite::Connection::open(path).unwrap();
        let mode: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode.to_ascii_lowercase(), "wal");
    }

    #[tokio::test]
    async fn same_key_with_a_different_hash_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let repository = IdempotencyRepository::open(temp.path().join("memory.sqlite"))
            .await
            .unwrap();
        repository
            .reserve(&[entry("hash-a")], "run-1")
            .await
            .unwrap();

        let error = repository
            .reserve(&[entry("hash-b")], "run-2")
            .await
            .unwrap_err();
        assert_eq!(error, IdempotencyError::Conflict);
    }
}
