use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use crate::graph::{stable_input_hash, GraphInputHashFields, GraphMemoryRecord, IngestionRun};
use crate::{GraphAddMemoryRequest, GraphAddMemoryResponse, MemoryError, MemoryResult};

const GRAPH_PIPELINE_VERSION: &str = "graph-pipeline-v1";

#[derive(Clone, Debug)]
pub struct GraphRepository {
    path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct ClaimedIngestionRun {
    pub ingestion_run_id: String,
    pub memory_space_id: String,
    pub memory_record_id: String,
    pub text: String,
    pub attempt_count: i64,
}

#[derive(Clone, Debug)]
pub struct RecordEmbeddingUpdate {
    pub ingestion_run_id: String,
    pub memory_record_id: String,
    pub memory_space_id: String,
    pub attempt_count: i64,
    pub embedding: Vec<f32>,
    pub embedding_model: String,
    pub embedding_version: String,
}

impl GraphRepository {
    pub fn open(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub async fn accept_memory_record(
        &self,
        request: GraphAddMemoryRequest,
    ) -> MemoryResult<GraphAddMemoryResponse> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || accept_memory_record_sync(&path, request))
            .await
            .map_err(|error| MemoryError::StoreBackend {
                message: format!("sqlite task failed: {error}"),
            })?
    }

    pub async fn claim_pending_run(
        &self,
        ingestion_run_id: &str,
    ) -> MemoryResult<ClaimedIngestionRun> {
        let path = self.path.clone();
        let ingestion_run_id = ingestion_run_id.to_string();
        run_sqlite_operation(move || claim_pending_run_sync(&path, &ingestion_run_id)).await
    }

    pub async fn store_record_embedding(&self, update: RecordEmbeddingUpdate) -> MemoryResult<()> {
        let path = self.path.clone();
        run_sqlite_operation(move || store_record_embedding_sync(&path, &update)).await
    }

    pub async fn mark_run_failed_if_current_attempt(
        &self,
        ingestion_run_id: &str,
        attempt_count: i64,
        stage: &str,
        error_code: &str,
        error_message: &str,
    ) -> MemoryResult<()> {
        let path = self.path.clone();
        let ingestion_run_id = ingestion_run_id.to_string();
        let stage = stage.to_string();
        let error_code = error_code.to_string();
        let error_message = error_message.to_string();
        run_sqlite_operation(move || {
            mark_run_failed_if_current_attempt_sync(
                &path,
                &ingestion_run_id,
                attempt_count,
                &stage,
                &error_code,
                &error_message,
            )
        })
        .await
    }

    pub async fn get_run(&self, ingestion_run_id: &str) -> MemoryResult<IngestionRun> {
        let path = self.path.clone();
        let ingestion_run_id = ingestion_run_id.to_string();
        run_sqlite_operation(move || get_run_sync(&path, &ingestion_run_id)).await
    }

    pub async fn get_graph_memory_record(
        &self,
        memory_record_id: &str,
    ) -> MemoryResult<GraphMemoryRecord> {
        let path = self.path.clone();
        let memory_record_id = memory_record_id.to_string();
        run_sqlite_operation(move || get_graph_memory_record_sync(&path, &memory_record_id)).await
    }

    pub async fn count_facts(&self, memory_space_id: &str) -> MemoryResult<i64> {
        let path = self.path.clone();
        let memory_space_id = memory_space_id.to_string();
        run_sqlite_operation(move || {
            let connection = open_graph_connection(&path)?;
            let count = connection.query_row(
                "SELECT COUNT(*) FROM graph_facts WHERE memory_space_id = ?1",
                params![&memory_space_id],
                |row| row.get(0),
            )?;
            Ok(count)
        })
        .await
    }
}

async fn run_sqlite_operation<T, F>(operation: F) -> MemoryResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> MemoryResult<T> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| MemoryError::StoreBackend {
            message: format!("sqlite task failed: {error}"),
        })?
}

fn open_graph_connection(path: &Path) -> MemoryResult<Connection> {
    if let Some(parent) = parent_dir_to_create(path) {
        std::fs::create_dir_all(parent)?;
    }
    let connection = Connection::open(path)?;
    connection.busy_timeout(Duration::from_millis(5_000))?;
    if !is_memory_database(path) {
        let _: String = connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    }
    crate::sqlite::initialize_schema(&connection)?;
    Ok(connection)
}

fn parent_dir_to_create(path: &Path) -> Option<&Path> {
    if is_memory_database(path) {
        return None;
    }
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
}

fn is_memory_database(path: &Path) -> bool {
    path == Path::new(":memory:")
}

fn accept_memory_record_sync(
    path: &Path,
    request: GraphAddMemoryRequest,
) -> MemoryResult<GraphAddMemoryResponse> {
    if request.text.trim().is_empty() {
        return Err(MemoryError::InvalidInput {
            message: "memory text must not be empty".to_string(),
        });
    }

    let mut connection = open_graph_connection(path)?;
    let transaction = connection.transaction()?;
    let now = current_time_ms();

    transaction.execute(
        "INSERT OR IGNORE INTO graph_memory_spaces
         (id, owner_id, status, next_ingestion_sequence, created_at_ms)
         VALUES (?1, ?2, 'active', 1, ?3)",
        params![&request.memory_space_id, &request.owner_id, now as i64],
    )?;
    let stored_owner_id: String = transaction.query_row(
        "SELECT owner_id FROM graph_memory_spaces WHERE id = ?1",
        params![&request.memory_space_id],
        |row| row.get(0),
    )?;
    if stored_owner_id != request.owner_id {
        return Err(MemoryError::InvalidInput {
            message: "MEMORY_SPACE_OWNER_MISMATCH".to_string(),
        });
    }

    let input_hash = stable_input_hash(&GraphInputHashFields {
        memory_space_id: request.memory_space_id.clone(),
        session_id: request.session_id.clone(),
        session_sequence: request.session_sequence,
        text: request.text.clone(),
        source_kind: request.source_kind.clone(),
        source_ref: request.source_ref.clone(),
        content_role: request.content_role.clone(),
        observed_at_ms: request.observed_at_ms,
        metadata: request.metadata.clone(),
    });

    let existing = transaction
        .query_row(
            "SELECT memory_record_id, id, input_hash, status
             FROM graph_ingestion_runs
             WHERE memory_space_id = ?1 AND idempotency_key = ?2",
            params![&request.memory_space_id, &request.idempotency_key],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;

    if let Some((memory_record_id, ingestion_run_id, existing_hash, status)) = existing {
        if existing_hash != input_hash {
            return Err(MemoryError::InvalidInput {
                message: "IDEMPOTENCY_CONFLICT".to_string(),
            });
        }
        transaction.commit()?;
        return Ok(GraphAddMemoryResponse {
            memory_record_id,
            ingestion_run_id,
            status,
            vector_ready: false,
            graph_ready: false,
        });
    }

    let memory_record_id = Uuid::new_v4().to_string();
    let ingestion_run_id = Uuid::new_v4().to_string();
    let ingestion_sequence: i64 = transaction.query_row(
        "SELECT next_ingestion_sequence FROM graph_memory_spaces WHERE id = ?1",
        params![&request.memory_space_id],
        |row| row.get(0),
    )?;

    transaction.execute(
        "UPDATE graph_memory_spaces
         SET next_ingestion_sequence = next_ingestion_sequence + 1
         WHERE id = ?1",
        params![&request.memory_space_id],
    )?;

    transaction.execute(
        "INSERT INTO graph_memory_records (
            id, memory_space_id, session_id, ingestion_sequence, session_sequence,
            text, metadata_json, source_kind, source_ref, content_role,
            created_by_agent_id, observed_at_ms, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
        params![
            &memory_record_id,
            &request.memory_space_id,
            &request.session_id,
            ingestion_sequence,
            request.session_sequence,
            &request.text,
            serde_json::to_string(&request.metadata)?,
            &request.source_kind,
            &request.source_ref,
            &request.content_role,
            &request.created_by_agent_id,
            request.observed_at_ms.map(|value| value as i64),
            now as i64,
        ],
    )?;

    transaction.execute(
        "INSERT INTO graph_memory_record_fts(id, memory_space_id, text) VALUES (?1, ?2, ?3)",
        params![&memory_record_id, &request.memory_space_id, &request.text],
    )?;

    transaction.execute(
        "INSERT INTO graph_ingestion_runs (
            id, memory_space_id, memory_record_id, idempotency_key, input_hash,
            status, stage, attempt_count, pipeline_version, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', 'accepted', 0, ?6, ?7, ?7)",
        params![
            &ingestion_run_id,
            &request.memory_space_id,
            &memory_record_id,
            &request.idempotency_key,
            &input_hash,
            GRAPH_PIPELINE_VERSION,
            now as i64,
        ],
    )?;

    transaction.commit()?;
    Ok(GraphAddMemoryResponse {
        memory_record_id,
        ingestion_run_id,
        status: "pending".to_string(),
        vector_ready: false,
        graph_ready: false,
    })
}

fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn claim_pending_run_sync(
    path: &Path,
    ingestion_run_id: &str,
) -> MemoryResult<ClaimedIngestionRun> {
    let mut connection = open_graph_connection(path)?;
    let transaction = connection.transaction()?;
    let (memory_space_id, memory_record_id, text, status, attempt_count) = transaction.query_row(
        "SELECT runs.memory_space_id, runs.memory_record_id, records.text, runs.status, runs.attempt_count
         FROM graph_ingestion_runs runs
         JOIN graph_memory_records records
           ON records.id = runs.memory_record_id
          AND records.memory_space_id = runs.memory_space_id
         WHERE runs.id = ?1",
        params![ingestion_run_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        },
    )?;

    if status != "pending" {
        return Err(MemoryError::InvalidInput {
            message: format!("ingestion run is not pending: {status}"),
        });
    }

    let next_attempt = attempt_count + 1;
    let now = current_time_ms() as i64;
    let updated = transaction.execute(
        "UPDATE graph_ingestion_runs
         SET status = 'running',
             stage = 'embedding',
             attempt_count = ?1,
             started_at_ms = COALESCE(started_at_ms, ?2),
             updated_at_ms = ?2
         WHERE id = ?3
           AND status = 'pending'
           AND attempt_count = ?4",
        params![next_attempt, now, ingestion_run_id, attempt_count],
    )?;
    if updated != 1 {
        return Err(MemoryError::StoreBackend {
            message: "failed to claim ingestion run".to_string(),
        });
    }

    transaction.commit()?;
    Ok(ClaimedIngestionRun {
        ingestion_run_id: ingestion_run_id.to_string(),
        memory_space_id,
        memory_record_id,
        text,
        attempt_count: next_attempt,
    })
}

fn store_record_embedding_sync(path: &Path, update: &RecordEmbeddingUpdate) -> MemoryResult<()> {
    let mut connection = open_graph_connection(path)?;
    let transaction = connection.transaction()?;
    let now = current_time_ms() as i64;
    let updated_runs = transaction.execute(
        "UPDATE graph_ingestion_runs
         SET status = 'running',
             stage = 'extraction',
             updated_at_ms = ?1
         WHERE id = ?2
           AND memory_space_id = ?3
           AND attempt_count = ?4
           AND status = 'running'
           AND stage = 'embedding'
           AND memory_record_id = ?5",
        params![
            now,
            &update.ingestion_run_id,
            &update.memory_space_id,
            update.attempt_count,
            &update.memory_record_id
        ],
    )?;
    if updated_runs != 1 {
        return Err(MemoryError::StoreBackend {
            message: "ingestion run attempt is no longer current".to_string(),
        });
    }

    let updated_records = transaction.execute(
        "UPDATE graph_memory_records
         SET embedding = ?1,
             embedding_dims = ?2,
             embedding_model = ?3,
             embedding_version = ?4,
             updated_at_ms = ?5
         WHERE id = ?6
           AND memory_space_id = ?7",
        params![
            embedding_to_blob(&update.embedding),
            update.embedding.len() as i64,
            &update.embedding_model,
            &update.embedding_version,
            now,
            &update.memory_record_id,
            &update.memory_space_id,
        ],
    )?;
    if updated_records != 1 {
        return Err(MemoryError::StoreBackend {
            message: "graph memory record not found for embedding update".to_string(),
        });
    }

    transaction.commit()?;
    Ok(())
}

fn mark_run_failed_if_current_attempt_sync(
    path: &Path,
    ingestion_run_id: &str,
    attempt_count: i64,
    stage: &str,
    error_code: &str,
    error_message: &str,
) -> MemoryResult<()> {
    let mut connection = open_graph_connection(path)?;
    let transaction = connection.transaction()?;
    let now = current_time_ms() as i64;
    let updated = transaction.execute(
        "UPDATE graph_ingestion_runs
         SET status = 'failed',
             stage = ?1,
             error_code = ?2,
             error_message = ?3,
             updated_at_ms = ?4,
             completed_at_ms = ?4
         WHERE id = ?5
           AND attempt_count = ?6
           AND status = 'running'
           AND stage = ?1",
        params![
            stage,
            error_code,
            error_message,
            now,
            ingestion_run_id,
            attempt_count
        ],
    )?;
    if updated != 1 {
        return Err(MemoryError::StoreBackend {
            message: "failed to mark ingestion run failed".to_string(),
        });
    }
    transaction.commit()?;
    Ok(())
}

fn get_run_sync(path: &Path, ingestion_run_id: &str) -> MemoryResult<IngestionRun> {
    let connection = open_graph_connection(path)?;
    connection
        .query_row(
            "SELECT id, memory_space_id, memory_record_id, idempotency_key, input_hash,
                    status, stage, attempt_count, pipeline_version, error_code, error_message,
                    created_at_ms, started_at_ms, updated_at_ms, completed_at_ms
             FROM graph_ingestion_runs
             WHERE id = ?1",
            params![ingestion_run_id],
            |row| {
                Ok(IngestionRun {
                    id: row.get(0)?,
                    memory_space_id: row.get(1)?,
                    memory_record_id: row.get(2)?,
                    idempotency_key: row.get(3)?,
                    input_hash: row.get(4)?,
                    status: row.get(5)?,
                    stage: row.get(6)?,
                    attempt_count: row.get(7)?,
                    pipeline_version: row.get(8)?,
                    error_code: row.get(9)?,
                    error_message: row.get(10)?,
                    created_at_ms: row.get::<_, i64>(11)? as u64,
                    started_at_ms: row.get::<_, Option<i64>>(12)?.map(|value| value as u64),
                    updated_at_ms: row.get::<_, i64>(13)? as u64,
                    completed_at_ms: row.get::<_, Option<i64>>(14)?.map(|value| value as u64),
                })
            },
        )
        .map_err(Into::into)
}

fn get_graph_memory_record_sync(
    path: &Path,
    memory_record_id: &str,
) -> MemoryResult<GraphMemoryRecord> {
    let connection = open_graph_connection(path)?;
    let (
        id,
        memory_space_id,
        session_id,
        ingestion_sequence,
        session_sequence,
        text,
        metadata_json,
        source_kind,
        source_ref,
        content_role,
        created_by_agent_id,
        observed_at_ms,
        embedding_blob,
        embedding_dims,
        embedding_model,
        embedding_version,
        created_at_ms,
        updated_at_ms,
        deleted_at_ms,
    ) = connection.query_row(
        "SELECT id, memory_space_id, session_id, ingestion_sequence, session_sequence,
                text, metadata_json, source_kind, source_ref, content_role,
                created_by_agent_id, observed_at_ms, embedding, embedding_dims,
                embedding_model, embedding_version, created_at_ms, updated_at_ms, deleted_at_ms
         FROM graph_memory_records
         WHERE id = ?1",
        params![memory_record_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, Option<i64>>(11)?,
                row.get::<_, Option<Vec<u8>>>(12)?,
                row.get::<_, Option<i64>>(13)?,
                row.get::<_, Option<String>>(14)?,
                row.get::<_, Option<String>>(15)?,
                row.get::<_, i64>(16)?,
                row.get::<_, i64>(17)?,
                row.get::<_, Option<i64>>(18)?,
            ))
        },
    )?;

    Ok(GraphMemoryRecord {
        id: id.clone(),
        memory_space_id,
        session_id,
        ingestion_sequence,
        session_sequence,
        text,
        metadata: serde_json::from_str(&metadata_json)?,
        source_kind,
        source_ref,
        content_role,
        created_by_agent_id,
        observed_at_ms: observed_at_ms.map(|value| value as u64),
        embedding: blob_to_embedding(embedding_blob, embedding_dims, &id)?,
        embedding_model,
        embedding_version,
        created_at_ms: created_at_ms as u64,
        updated_at_ms: updated_at_ms as u64,
        deleted_at_ms: deleted_at_ms.map(|value| value as u64),
    })
}

fn embedding_to_blob(embedding: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(embedding.len() * 4);
    for value in embedding {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn blob_to_embedding(
    bytes: Option<Vec<u8>>,
    expected_dims: Option<i64>,
    record_id: &str,
) -> MemoryResult<Option<Vec<f32>>> {
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    if bytes.len() % 4 != 0 {
        return Err(MemoryError::StoreBackend {
            message: format!(
                "invalid embedding blob length for graph record '{record_id}': {}",
                bytes.len()
            ),
        });
    }
    let embedding = bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect::<Vec<_>>();
    if let Some(expected_dims) = expected_dims {
        if embedding.len() != expected_dims as usize {
            return Err(MemoryError::StoreBackend {
                message: format!(
                    "embedding dims mismatch for graph record '{record_id}': blob has {} dims but stored dims is {expected_dims}",
                    embedding.len()
                ),
            });
        }
    }
    Ok(Some(embedding))
}

#[cfg(test)]
mod tests {
    use super::open_graph_connection;
    use std::path::Path;

    #[test]
    fn graph_connection_uses_busy_timeout_and_wal() {
        let temp = tempfile::tempdir().unwrap();
        let connection = open_graph_connection(&temp.path().join("graph.sqlite")).unwrap();

        let busy_timeout_ms: i64 = connection
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert!(busy_timeout_ms >= 5_000);

        let journal_mode: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode.to_lowercase(), "wal");
    }

    #[test]
    fn graph_connection_supports_memory_database() {
        let connection = open_graph_connection(Path::new(":memory:")).unwrap();

        let busy_timeout_ms: i64 = connection
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert!(busy_timeout_ms >= 5_000);

        connection
            .query_row(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'graph_memory_spaces'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
    }

    #[test]
    fn graph_connection_skips_empty_parent_paths() {
        assert!(super::parent_dir_to_create(Path::new(":memory:")).is_none());
        assert!(super::parent_dir_to_create(Path::new("graph.sqlite")).is_none());
        assert_eq!(
            super::parent_dir_to_create(Path::new("data/graph.sqlite")),
            Some(Path::new("data"))
        );
    }
}
