use std::{
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, ErrorCode, OptionalExtension, TransactionBehavior};
use uuid::Uuid;

use crate::model::{Chunk, Dataset, Document, IngestionTask, StoredDocument};

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(30);
const SQLITE_LOCK_RETRY_ATTEMPTS: usize = 3;
const SQLITE_LOCK_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(50);
const SQLITE_LOCK_RETRY_MAX_DELAY: Duration = Duration::from_millis(500);

pub struct RagRepository {
    path: PathBuf,
}

impl RagRepository {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn initialize(&self) -> Result<()> {
        retry_sqlite_locked(|| {
            let connection = open_connection(&self.path)?;
            initialize_schema(&connection)
        })
    }

    pub fn create_dataset(
        &self,
        id: Option<&str>,
        name: &str,
        description: Option<&str>,
    ) -> Result<Dataset> {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "dataset name must not be empty");

        retry_sqlite_locked(|| {
            let connection = open_connection(&self.path)?;
            let now = current_time_ms();
            let dataset = Dataset {
                id: request_id_or_uuid(id, "dataset id")?,
                name: name.to_string(),
                description: description.map(str::to_string),
                created_at_ms: now,
                updated_at_ms: now,
            };

            connection.execute(
                r#"
                INSERT INTO rag_datasets (id, name, description, created_at_ms, updated_at_ms)
                VALUES (?1, ?2, ?3, ?4, ?5)
                "#,
                params![
                    dataset.id,
                    dataset.name,
                    dataset.description,
                    dataset.created_at_ms as i64,
                    dataset.updated_at_ms as i64,
                ],
            )?;

            Ok(dataset)
        })
    }

    pub fn list_datasets(&self) -> Result<Vec<Dataset>> {
        retry_sqlite_locked(|| {
            let connection = open_connection(&self.path)?;
            let mut statement = connection.prepare(
                r#"
                SELECT id, name, description, created_at_ms, updated_at_ms
                FROM rag_datasets
                ORDER BY created_at_ms DESC, id ASC
                "#,
            )?;
            let rows = statement.query_map([], read_dataset)?;
            collect_rows(rows)
        })
    }

    pub fn get_dataset(&self, dataset_id: &str) -> Result<Option<Dataset>> {
        retry_sqlite_locked(|| {
            let connection = open_connection(&self.path)?;
            connection
                .query_row(
                    r#"
                    SELECT id, name, description, created_at_ms, updated_at_ms
                    FROM rag_datasets
                    WHERE id = ?1
                    "#,
                    params![dataset_id],
                    read_dataset,
                )
                .optional()
                .map_err(Into::into)
        })
    }

    pub fn create_document_with_task(
        &self,
        dataset_id: &str,
        document_id: Option<&str>,
        task_id: Option<&str>,
        name: &str,
        file_path: &str,
        mime_type: Option<&str>,
        size_bytes: u64,
    ) -> Result<(Document, IngestionTask)> {
        let name = name.trim();
        let file_path = file_path.trim();
        anyhow::ensure!(!name.is_empty(), "document name must not be empty");
        anyhow::ensure!(
            !file_path.is_empty(),
            "document file path must not be empty"
        );

        retry_sqlite_locked(|| {
            let mut connection = open_connection(&self.path)?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let dataset_exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM rag_datasets WHERE id = ?1)",
                params![dataset_id],
                |row| row.get(0),
            )?;
            anyhow::ensure!(dataset_exists, "dataset not found");

            let now = current_time_ms();
            let document = Document {
                id: request_id_or_uuid(document_id, "document id")?,
                dataset_id: dataset_id.to_string(),
                name: name.to_string(),
                file_path: file_path.to_string(),
                mime_type: mime_type.map(str::to_string),
                size_bytes,
                status: "uploaded".to_string(),
                chunk_count: 0,
                error: None,
                created_at_ms: now,
                updated_at_ms: now,
            };
            let task = IngestionTask {
                id: request_id_or_uuid(task_id, "task id")?,
                dataset_id: dataset_id.to_string(),
                document_id: document.id.clone(),
                status: "pending".to_string(),
                error: None,
                created_at_ms: now,
                updated_at_ms: now,
                started_at_ms: None,
                completed_at_ms: None,
            };

            transaction.execute(
                r#"
                INSERT INTO rag_documents (
                    id,
                    dataset_id,
                    name,
                    file_path,
                    mime_type,
                    size_bytes,
                    status,
                    chunk_count,
                    error,
                    created_at_ms,
                    updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, NULL, ?8, ?9)
                "#,
                params![
                    document.id,
                    document.dataset_id,
                    document.name,
                    document.file_path,
                    document.mime_type,
                    document.size_bytes as i64,
                    document.status,
                    document.created_at_ms as i64,
                    document.updated_at_ms as i64,
                ],
            )?;
            transaction.execute(
                r#"
                INSERT INTO rag_tasks (
                    id, dataset_id, document_id, status, error, created_at_ms, updated_at_ms, started_at_ms, completed_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, NULL, NULL)
                "#,
                params![
                    task.id,
                    task.dataset_id,
                    task.document_id,
                    task.status,
                    task.created_at_ms as i64,
                    task.updated_at_ms as i64,
                ],
            )?;
            transaction.commit()?;

            Ok((document, task))
        })
    }

    pub fn list_documents(&self, dataset_id: &str) -> Result<Vec<Document>> {
        retry_sqlite_locked(|| {
            let connection = open_connection(&self.path)?;
            let mut statement = connection.prepare(
                r#"
                SELECT
                    id,
                    dataset_id,
                    name,
                    file_path,
                    mime_type,
                    size_bytes,
                    status,
                    chunk_count,
                    error,
                    created_at_ms,
                    updated_at_ms
                FROM rag_documents
                WHERE dataset_id = ?1
                ORDER BY created_at_ms DESC, id ASC
                "#,
            )?;
            let rows = statement.query_map(params![dataset_id], read_document)?;
            collect_rows(rows)
        })
    }

    pub fn update_document_with_task(
        &self,
        dataset_id: &str,
        document_id: &str,
        task_id: Option<&str>,
        name: &str,
        file_path: &str,
        mime_type: Option<&str>,
        size_bytes: u64,
    ) -> Result<(Document, IngestionTask)> {
        let name = name.trim();
        let file_path = file_path.trim();
        anyhow::ensure!(!name.is_empty(), "document name must not be empty");
        anyhow::ensure!(
            !file_path.is_empty(),
            "document file path must not be empty"
        );

        retry_sqlite_locked(|| {
            let mut connection = open_connection(&self.path)?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let existing = transaction
                .query_row(
                    r#"
                    SELECT
                        id,
                        dataset_id,
                        name,
                        file_path,
                        mime_type,
                        size_bytes,
                        status,
                        chunk_count,
                        error,
                        created_at_ms,
                        updated_at_ms
                    FROM rag_documents
                    WHERE id = ?1 AND dataset_id = ?2
                    "#,
                    params![document_id, dataset_id],
                    read_document,
                )
                .optional()?;
            let Some(existing) = existing else {
                transaction.commit()?;
                anyhow::bail!("document not found");
            };

            let now = current_time_ms();
            let document = Document {
                id: existing.id,
                dataset_id: existing.dataset_id,
                name: name.to_string(),
                file_path: file_path.to_string(),
                mime_type: mime_type.map(str::to_string),
                size_bytes,
                status: "uploaded".to_string(),
                chunk_count: 0,
                error: None,
                created_at_ms: existing.created_at_ms,
                updated_at_ms: now,
            };
            let task = IngestionTask {
                id: request_id_or_uuid(task_id, "task id")?,
                dataset_id: dataset_id.to_string(),
                document_id: document_id.to_string(),
                status: "pending".to_string(),
                error: None,
                created_at_ms: now,
                updated_at_ms: now,
                started_at_ms: None,
                completed_at_ms: None,
            };

            transaction.execute(
                r#"
                UPDATE rag_documents
                SET name = ?3,
                    file_path = ?4,
                    mime_type = ?5,
                    size_bytes = ?6,
                    status = 'uploaded',
                    chunk_count = 0,
                    error = NULL,
                    updated_at_ms = ?7
                WHERE id = ?1 AND dataset_id = ?2
                "#,
                params![
                    document_id,
                    dataset_id,
                    document.name,
                    document.file_path,
                    document.mime_type,
                    document.size_bytes as i64,
                    document.updated_at_ms as i64,
                ],
            )?;
            transaction.execute(
                "DELETE FROM rag_chunks WHERE dataset_id = ?1 AND document_id = ?2",
                params![dataset_id, document_id],
            )?;
            transaction.execute(
                "DELETE FROM rag_tasks WHERE dataset_id = ?1 AND document_id = ?2 AND status = 'pending'",
                params![dataset_id, document_id],
            )?;
            transaction.execute(
                r#"
                INSERT INTO rag_tasks (
                    id, dataset_id, document_id, status, error, created_at_ms, updated_at_ms, started_at_ms, completed_at_ms
                )
                VALUES (?1, ?2, ?3, 'pending', NULL, ?4, ?5, NULL, NULL)
                "#,
                params![
                    task.id,
                    task.dataset_id,
                    task.document_id,
                    task.created_at_ms as i64,
                    task.updated_at_ms as i64,
                ],
            )?;
            transaction.commit()?;

            Ok((document, task))
        })
    }

    pub fn delete_document(&self, dataset_id: &str, document_id: &str) -> Result<Option<Document>> {
        retry_sqlite_locked(|| {
            let mut connection = open_connection(&self.path)?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let document = transaction
                .query_row(
                    r#"
                    SELECT
                        id,
                        dataset_id,
                        name,
                        file_path,
                        mime_type,
                        size_bytes,
                        status,
                        chunk_count,
                        error,
                        created_at_ms,
                        updated_at_ms
                    FROM rag_documents
                    WHERE id = ?1 AND dataset_id = ?2
                    "#,
                    params![document_id, dataset_id],
                    read_document,
                )
                .optional()?;
            if document.is_none() {
                transaction.commit()?;
                return Ok(None);
            }

            transaction.execute(
                "DELETE FROM rag_chunks WHERE dataset_id = ?1 AND document_id = ?2",
                params![dataset_id, document_id],
            )?;
            transaction.execute(
                "DELETE FROM rag_tasks WHERE dataset_id = ?1 AND document_id = ?2",
                params![dataset_id, document_id],
            )?;
            transaction.execute(
                "DELETE FROM rag_documents WHERE id = ?1 AND dataset_id = ?2",
                params![document_id, dataset_id],
            )?;
            transaction.commit()?;

            Ok(document)
        })
    }

    pub fn get_task(&self, task_id: &str) -> Result<Option<IngestionTask>> {
        retry_sqlite_locked(|| {
            let connection = open_connection(&self.path)?;
            connection
                .query_row(
                    r#"
                    SELECT id, dataset_id, document_id, status, error, created_at_ms, updated_at_ms, started_at_ms, completed_at_ms
                    FROM rag_tasks
                    WHERE id = ?1
                    "#,
                    params![task_id],
                    read_task,
                )
                .optional()
                .map_err(Into::into)
        })
    }

    pub fn lease_next_pending_task(&self) -> Result<Option<IngestionTask>> {
        retry_sqlite_locked(|| {
            let mut connection = open_connection(&self.path)?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let task = transaction
                .query_row(
                    r#"
                    SELECT id, dataset_id, document_id, status, error, created_at_ms, updated_at_ms, started_at_ms, completed_at_ms
                    FROM rag_tasks
                    WHERE status = 'pending'
                    ORDER BY created_at_ms ASC, id ASC
                    LIMIT 1
                    "#,
                    [],
                    read_task,
                )
                .optional()?;

            let Some(mut task) = task else {
                transaction.commit()?;
                return Ok(None);
            };

            let now = current_time_ms();
            let changed = transaction.execute(
                r#"
                UPDATE rag_tasks
                SET status = 'running', updated_at_ms = ?2, started_at_ms = ?2
                WHERE id = ?1 AND status = 'pending'
                "#,
                params![task.id, now as i64],
            )?;
            if changed == 0 {
                transaction.commit()?;
                return Ok(None);
            }
            transaction.execute(
                r#"
                UPDATE rag_documents
                SET status = 'running', error = NULL, updated_at_ms = ?2
                WHERE id = ?1
                "#,
                params![task.document_id, now as i64],
            )?;
            transaction.commit()?;

            task.status = "running".to_string();
            task.updated_at_ms = now;
            task.started_at_ms = Some(now);
            Ok(Some(task))
        })
    }

    pub fn get_stored_document(&self, document_id: &str) -> Result<Option<StoredDocument>> {
        retry_sqlite_locked(|| {
            let connection = open_connection(&self.path)?;
            connection
                .query_row(
                    r#"
                    SELECT id, dataset_id, name, file_path, mime_type
                    FROM rag_documents
                    WHERE id = ?1
                    "#,
                    params![document_id],
                    |row| {
                        Ok(StoredDocument {
                            id: row.get(0)?,
                            dataset_id: row.get(1)?,
                            name: row.get(2)?,
                            file_path: row.get(3)?,
                            mime_type: row.get(4)?,
                        })
                    },
                )
                .optional()
                .map_err(Into::into)
        })
    }

    pub fn replace_chunks(
        &self,
        dataset_id: &str,
        document_id: &str,
        chunks: &[Chunk],
    ) -> Result<()> {
        retry_sqlite_locked(|| {
            let mut connection = open_connection(&self.path)?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute(
                "DELETE FROM rag_chunks WHERE dataset_id = ?1 AND document_id = ?2",
                params![dataset_id, document_id],
            )?;

            for chunk in chunks {
                transaction.execute(
                    r#"
                    INSERT INTO rag_chunks (
                        id,
                        dataset_id,
                        document_id,
                        chunk_index,
                        content,
                        chunk_type,
                        token_count,
                        parse_topology,
                        source_node_indices_json,
                        available,
                        created_at_ms,
                        updated_at_ms
                    )
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                    "#,
                    params![
                        chunk.id,
                        chunk.dataset_id,
                        chunk.document_id,
                        chunk.chunk_index as i64,
                        chunk.content,
                        chunk.chunk_type,
                        chunk.token_count as i64,
                        chunk.parse_topology,
                        serde_json::to_string(&chunk.source_node_indices)?,
                        i64::from(chunk.available),
                        chunk.created_at_ms as i64,
                        chunk.updated_at_ms as i64,
                    ],
                )?;
            }

            transaction.commit()?;
            Ok(())
        })
    }

    pub fn complete_task(
        &self,
        task_id: &str,
        document_id: &str,
        chunk_count: usize,
    ) -> Result<()> {
        retry_sqlite_locked(|| {
            let mut connection = open_connection(&self.path)?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = current_time_ms();
            transaction.execute(
                r#"
                UPDATE rag_tasks
                SET status = 'completed', error = NULL, updated_at_ms = ?2, completed_at_ms = ?2
                WHERE id = ?1
                "#,
                params![task_id, now as i64],
            )?;
            transaction.execute(
                r#"
                UPDATE rag_documents
                SET status = 'completed', chunk_count = ?2, error = NULL, updated_at_ms = ?3
                WHERE id = ?1
                "#,
                params![document_id, chunk_count as i64, now as i64],
            )?;
            transaction.commit()?;
            Ok(())
        })
    }

    pub fn fail_task(&self, task_id: &str, document_id: &str, error: &str) -> Result<()> {
        retry_sqlite_locked(|| {
            let mut connection = open_connection(&self.path)?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = current_time_ms();
            transaction.execute(
                r#"
                UPDATE rag_tasks
                SET status = 'failed', error = ?2, updated_at_ms = ?3, completed_at_ms = ?3
                WHERE id = ?1
                "#,
                params![task_id, error, now as i64],
            )?;
            transaction.execute(
                r#"
                UPDATE rag_documents
                SET status = 'failed', error = ?2, updated_at_ms = ?3
                WHERE id = ?1
                "#,
                params![document_id, error, now as i64],
            )?;
            transaction.commit()?;
            Ok(())
        })
    }

    pub fn list_chunks(&self, dataset_id: &str, document_id: &str) -> Result<Vec<Chunk>> {
        retry_sqlite_locked(|| {
            let connection = open_connection(&self.path)?;
            let mut statement = connection.prepare(
                r#"
                SELECT
                    id,
                    dataset_id,
                    document_id,
                    chunk_index,
                    content,
                    chunk_type,
                    token_count,
                    parse_topology,
                    source_node_indices_json,
                    available,
                    created_at_ms,
                    updated_at_ms
                FROM rag_chunks
                WHERE dataset_id = ?1 AND document_id = ?2
                ORDER BY chunk_index ASC
                "#,
            )?;
            let rows = statement.query_map(params![dataset_id, document_id], read_chunk)?;
            collect_rows(rows)
        })
    }
}

fn open_connection(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let connection =
        Connection::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    configure_connection(&connection)?;
    Ok(connection)
}

fn configure_connection(connection: &Connection) -> Result<()> {
    connection
        .busy_timeout(SQLITE_BUSY_TIMEOUT)
        .context("failed to configure sqlite busy timeout")?;
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .context("failed to enable sqlite WAL journal mode")?;
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .context("failed to configure sqlite synchronous mode")?;
    connection
        .pragma_update(None, "foreign_keys", 1)
        .context("failed to enable sqlite foreign keys")?;
    Ok(())
}

fn retry_sqlite_locked<T>(mut operation: impl FnMut() -> Result<T>) -> Result<T> {
    let mut delay = SQLITE_LOCK_RETRY_INITIAL_DELAY;
    for attempt in 0..=SQLITE_LOCK_RETRY_ATTEMPTS {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) => {
                if !is_retryable_sqlite_lock(&error) || attempt == SQLITE_LOCK_RETRY_ATTEMPTS {
                    return Err(error);
                }
                thread::sleep(delay);
                let next_delay_ms = delay
                    .as_millis()
                    .saturating_mul(2)
                    .min(SQLITE_LOCK_RETRY_MAX_DELAY.as_millis());
                delay = Duration::from_millis(next_delay_ms as u64);
            }
        }
    }
    unreachable!("sqlite retry loop always returns");
}

fn is_retryable_sqlite_lock(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        let Some(sqlite_error) = cause.downcast_ref::<rusqlite::Error>() else {
            return false;
        };
        matches!(
            sqlite_error.sqlite_error_code(),
            Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
        )
    })
}

fn initialize_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;

        CREATE TABLE IF NOT EXISTS rag_datasets (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            description TEXT,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS rag_documents (
            id TEXT PRIMARY KEY,
            dataset_id TEXT NOT NULL,
            name TEXT NOT NULL,
            file_path TEXT NOT NULL,
            mime_type TEXT,
            size_bytes INTEGER NOT NULL,
            status TEXT NOT NULL,
            chunk_count INTEGER NOT NULL DEFAULT 0,
            error TEXT,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            FOREIGN KEY(dataset_id) REFERENCES rag_datasets(id)
        );

        CREATE TABLE IF NOT EXISTS rag_tasks (
            id TEXT PRIMARY KEY,
            dataset_id TEXT NOT NULL,
            document_id TEXT NOT NULL,
            status TEXT NOT NULL,
            error TEXT,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            started_at_ms INTEGER,
            completed_at_ms INTEGER,
            FOREIGN KEY(dataset_id) REFERENCES rag_datasets(id),
            FOREIGN KEY(document_id) REFERENCES rag_documents(id)
        );

        CREATE TABLE IF NOT EXISTS rag_chunks (
            id TEXT PRIMARY KEY,
            dataset_id TEXT NOT NULL,
            document_id TEXT NOT NULL,
            chunk_index INTEGER NOT NULL,
            content TEXT NOT NULL,
            chunk_type TEXT NOT NULL DEFAULT 'text',
            token_count INTEGER NOT NULL DEFAULT 0,
            parse_topology TEXT NOT NULL DEFAULT 'list',
            source_node_indices_json TEXT NOT NULL DEFAULT '[]',
            available INTEGER NOT NULL,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            FOREIGN KEY(dataset_id) REFERENCES rag_datasets(id),
            FOREIGN KEY(document_id) REFERENCES rag_documents(id)
        );

        CREATE INDEX IF NOT EXISTS idx_rag_documents_dataset
        ON rag_documents(dataset_id);

        CREATE INDEX IF NOT EXISTS idx_rag_tasks_status
        ON rag_tasks(status, created_at_ms);

        CREATE INDEX IF NOT EXISTS idx_rag_chunks_document
        ON rag_chunks(dataset_id, document_id, chunk_index);
        "#,
    )?;
    ensure_column(
        connection,
        "rag_chunks",
        "chunk_type",
        "ALTER TABLE rag_chunks ADD COLUMN chunk_type TEXT NOT NULL DEFAULT 'text'",
    )?;
    ensure_column(
        connection,
        "rag_chunks",
        "token_count",
        "ALTER TABLE rag_chunks ADD COLUMN token_count INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        connection,
        "rag_chunks",
        "parse_topology",
        "ALTER TABLE rag_chunks ADD COLUMN parse_topology TEXT NOT NULL DEFAULT 'list'",
    )?;
    ensure_column(
        connection,
        "rag_chunks",
        "source_node_indices_json",
        "ALTER TABLE rag_chunks ADD COLUMN source_node_indices_json TEXT NOT NULL DEFAULT '[]'",
    )?;
    Ok(())
}

fn read_dataset(row: &rusqlite::Row<'_>) -> rusqlite::Result<Dataset> {
    Ok(Dataset {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        created_at_ms: read_u64(row, 3)?,
        updated_at_ms: read_u64(row, 4)?,
    })
}

fn read_document(row: &rusqlite::Row<'_>) -> rusqlite::Result<Document> {
    Ok(Document {
        id: row.get(0)?,
        dataset_id: row.get(1)?,
        name: row.get(2)?,
        file_path: row.get(3)?,
        mime_type: row.get(4)?,
        size_bytes: read_u64(row, 5)?,
        status: row.get(6)?,
        chunk_count: read_i64(row, 7)? as usize,
        error: row.get(8)?,
        created_at_ms: read_u64(row, 9)?,
        updated_at_ms: read_u64(row, 10)?,
    })
}

fn read_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<IngestionTask> {
    Ok(IngestionTask {
        id: row.get(0)?,
        dataset_id: row.get(1)?,
        document_id: row.get(2)?,
        status: row.get(3)?,
        error: row.get(4)?,
        created_at_ms: read_u64(row, 5)?,
        updated_at_ms: read_u64(row, 6)?,
        started_at_ms: read_optional_u64(row, 7)?,
        completed_at_ms: read_optional_u64(row, 8)?,
    })
}

fn read_chunk(row: &rusqlite::Row<'_>) -> rusqlite::Result<Chunk> {
    let available: i64 = row.get(9)?;
    let source_node_indices_json: String = row.get(8)?;
    let source_node_indices = serde_json::from_str(&source_node_indices_json).unwrap_or_default();
    Ok(Chunk {
        id: row.get(0)?,
        dataset_id: row.get(1)?,
        document_id: row.get(2)?,
        chunk_index: read_i64(row, 3)? as usize,
        content: row.get(4)?,
        chunk_type: row.get(5)?,
        token_count: read_i64(row, 6)? as usize,
        parse_topology: row.get(7)?,
        source_node_indices,
        available: available != 0,
        created_at_ms: read_u64(row, 10)?,
        updated_at_ms: read_u64(row, 11)?,
    })
}

fn ensure_column(
    connection: &Connection,
    table: &str,
    column: &str,
    alter_sql: &str,
) -> Result<()> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(());
        }
    }
    connection.execute(alter_sql, [])?;
    Ok(())
}

fn read_i64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<i64> {
    row.get(index)
}

fn read_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    Ok(value as u64)
}

fn read_optional_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
    let value: Option<i64> = row.get(index)?;
    Ok(value.map(|value| value as u64))
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>> {
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

pub fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn request_id_or_uuid(id: Option<&str>, label: &str) -> Result<String> {
    match id.map(str::trim) {
        Some(id) if id.is_empty() => anyhow::bail!("{label} must not be empty"),
        Some(id) => Ok(id.to_string()),
        None => Ok(Uuid::new_v4().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_repo() -> (RagRepository, PathBuf) {
        let path = std::env::temp_dir().join(format!("memory-cases-{}.sqlite", Uuid::new_v4()));
        let repo = RagRepository::new(&path);
        repo.initialize().expect("initialize repo");
        (repo, path)
    }

    fn remove_repo_files(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(sqlite_sidecar_path(path, "wal"));
        let _ = std::fs::remove_file(sqlite_sidecar_path(path, "shm"));
    }

    fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
        let mut path = path.as_os_str().to_os_string();
        path.push(format!("-{suffix}"));
        PathBuf::from(path)
    }

    fn sample_chunk(document_id: &str) -> Chunk {
        Chunk {
            id: format!("{document_id}_chunk_0"),
            dataset_id: "dataset-1".to_string(),
            document_id: document_id.to_string(),
            chunk_index: 0,
            content: "old content".to_string(),
            chunk_type: "text".to_string(),
            token_count: 2,
            parse_topology: "list".to_string(),
            source_node_indices: vec![0],
            available: true,
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    #[test]
    fn update_document_with_task_clears_chunks_and_enqueues_new_task() {
        let (repo, path) = test_repo();
        repo.create_dataset(Some("dataset-1"), "Dataset", None)
            .expect("create dataset");
        let (_document, old_task) = repo
            .create_document_with_task(
                "dataset-1",
                Some("doc-1"),
                Some("task-old"),
                "old.md",
                "/tmp/old.md",
                Some("text/markdown"),
                12,
            )
            .expect("create document");
        repo.replace_chunks("dataset-1", "doc-1", &[sample_chunk("doc-1")])
            .expect("replace chunks");

        let (updated, new_task) = repo
            .update_document_with_task(
                "dataset-1",
                "doc-1",
                Some("task-new"),
                "new.md",
                "/tmp/new.md",
                Some("text/markdown"),
                24,
            )
            .expect("update document");

        assert_eq!(updated.name, "new.md");
        assert_eq!(updated.file_path, "/tmp/new.md");
        assert_eq!(updated.status, "uploaded");
        assert_eq!(updated.chunk_count, 0);
        assert_eq!(new_task.id, "task-new");
        assert_eq!(new_task.status, "pending");
        assert!(repo
            .list_chunks("dataset-1", "doc-1")
            .expect("list chunks")
            .is_empty());
        assert!(repo.get_task(&old_task.id).expect("get old task").is_none());

        remove_repo_files(&path);
    }

    #[test]
    fn delete_document_removes_document_chunks_and_tasks() {
        let (repo, path) = test_repo();
        repo.create_dataset(Some("dataset-1"), "Dataset", None)
            .expect("create dataset");
        repo.create_document_with_task(
            "dataset-1",
            Some("doc-1"),
            Some("task-1"),
            "doc.md",
            "/tmp/doc.md",
            Some("text/markdown"),
            12,
        )
        .expect("create document");
        repo.replace_chunks("dataset-1", "doc-1", &[sample_chunk("doc-1")])
            .expect("replace chunks");

        let deleted = repo
            .delete_document("dataset-1", "doc-1")
            .expect("delete document")
            .expect("deleted document");

        assert_eq!(deleted.id, "doc-1");
        assert!(repo
            .get_stored_document("doc-1")
            .expect("get deleted document")
            .is_none());
        assert!(repo.get_task("task-1").expect("get deleted task").is_none());
        assert!(repo
            .list_chunks("dataset-1", "doc-1")
            .expect("list chunks")
            .is_empty());

        remove_repo_files(&path);
    }

    #[test]
    fn repository_waits_for_concurrent_sqlite_writer() {
        let (repo, path) = test_repo();
        repo.create_dataset(Some("dataset-1"), "Dataset", None)
            .expect("create dataset");

        let mut blocking_connection = Connection::open(&path).expect("open blocking connection");
        blocking_connection
            .busy_timeout(Duration::from_secs(1))
            .expect("configure blocking timeout");
        let transaction = blocking_connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .expect("start blocking transaction");
        transaction
            .execute(
                "UPDATE rag_datasets SET updated_at_ms = updated_at_ms WHERE id = ?1",
                params!["dataset-1"],
            )
            .expect("hold write lock");

        let locked_path = path.clone();
        let create_handle = std::thread::spawn(move || {
            let repo = RagRepository::new(locked_path);
            repo.create_document_with_task(
                "dataset-1",
                Some("doc-locked"),
                Some("task-locked"),
                "locked.md",
                "/tmp/locked.md",
                Some("text/markdown"),
                12,
            )
        });

        std::thread::sleep(Duration::from_millis(150));
        transaction.commit().expect("release write lock");

        let (document, task) = create_handle
            .join()
            .expect("join create thread")
            .expect("create document after lock releases");
        assert_eq!(document.id, "doc-locked");
        assert_eq!(task.id, "task-locked");

        remove_repo_files(&path);
    }
}
