use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use crate::graph::{stable_input_hash, GraphInputHashFields};
use crate::{GraphAddMemoryRequest, GraphAddMemoryResponse, MemoryError, MemoryResult};

const GRAPH_PIPELINE_VERSION: &str = "graph-pipeline-v1";

#[derive(Clone, Debug)]
pub struct GraphRepository {
    path: PathBuf,
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
