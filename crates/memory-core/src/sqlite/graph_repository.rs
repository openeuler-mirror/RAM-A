use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension, Transaction};
use uuid::Uuid;

use crate::graph::{
    normalize_graph_text, stable_input_hash, ExtractedEntityCandidate, ExtractedFactCandidate,
    ExtractionRun, GraphExtractionOutput, GraphInputHashFields, GraphMemoryRecord, IngestionRun,
};
use crate::{GraphAddMemoryRequest, GraphAddMemoryResponse, MemoryError, MemoryResult};

const GRAPH_PIPELINE_VERSION: &str = "graph-pipeline-v1";
const FACT_EVIDENCE_KIND_SUPPORT: &str = "support";
const RESOLUTION_METHOD_DETERMINISTIC: &str = "deterministic";

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

#[derive(Clone, Debug)]
pub struct ClaimedExtractionRun {
    pub ingestion_run_id: String,
    pub memory_space_id: String,
    pub memory_record_id: String,
    pub text: String,
    pub metadata: serde_json::Value,
    pub attempt_count: i64,
    pub extraction_attempt_number: i64,
    pub context_record_ids: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ExtractionRunCompletion {
    pub ingestion_run_id: String,
    pub memory_space_id: String,
    pub attempt_count: i64,
    pub attempt_number: i64,
    pub extractor_name: String,
    pub model: String,
    pub prompt_version: String,
    pub schema_version: String,
    pub type_registry_version: String,
    pub context_record_ids: Vec<String>,
    pub structured_output: serde_json::Value,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub latency_ms: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct ExtractionRunFailure {
    pub ingestion_run_id: String,
    pub memory_space_id: String,
    pub attempt_count: i64,
    pub attempt_number: i64,
    pub extractor_name: String,
    pub model: String,
    pub prompt_version: String,
    pub schema_version: String,
    pub type_registry_version: String,
    pub context_record_ids: Vec<String>,
    pub latency_ms: Option<i64>,
    pub error_code: String,
    pub error_message: String,
}

#[derive(Clone, Debug)]
pub struct ClaimedResolutionRun {
    pub ingestion_run_id: String,
    pub memory_space_id: String,
    pub memory_record_id: String,
    pub text: String,
    pub attempt_count: i64,
    pub extraction_run_id: String,
    pub extraction_output: GraphExtractionOutput,
}

#[derive(Clone, Debug)]
pub struct ResolutionPublishRequest {
    pub ingestion_run_id: String,
    pub memory_space_id: String,
    pub memory_record_id: String,
    pub extraction_run_id: String,
    pub attempt_count: i64,
    pub extraction_output: GraphExtractionOutput,
    pub type_registry_version: String,
    pub resolver_version: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolutionPublishResult {
    pub ingestion_run: IngestionRun,
    pub entities_created: usize,
    pub entities_reused: usize,
    pub facts_created: usize,
    pub facts_reused: usize,
    pub evidence_inserted: usize,
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

    pub async fn claim_extraction_run(
        &self,
        ingestion_run_id: &str,
    ) -> MemoryResult<ClaimedExtractionRun> {
        let path = self.path.clone();
        let ingestion_run_id = ingestion_run_id.to_string();
        run_sqlite_operation(move || claim_extraction_run_sync(&path, &ingestion_run_id)).await
    }

    pub async fn store_extraction_success(
        &self,
        completion: ExtractionRunCompletion,
    ) -> MemoryResult<ExtractionRun> {
        let path = self.path.clone();
        run_sqlite_operation(move || store_extraction_success_sync(&path, &completion)).await
    }

    pub async fn store_extraction_failure(
        &self,
        failure: ExtractionRunFailure,
    ) -> MemoryResult<ExtractionRun> {
        let path = self.path.clone();
        run_sqlite_operation(move || store_extraction_failure_sync(&path, &failure)).await
    }

    pub async fn claim_resolution_run(
        &self,
        ingestion_run_id: &str,
    ) -> MemoryResult<ClaimedResolutionRun> {
        let path = self.path.clone();
        let ingestion_run_id = ingestion_run_id.to_string();
        run_sqlite_operation(move || claim_resolution_run_sync(&path, &ingestion_run_id)).await
    }

    pub async fn publish_resolution(
        &self,
        request: ResolutionPublishRequest,
    ) -> MemoryResult<ResolutionPublishResult> {
        let path = self.path.clone();
        run_sqlite_operation(move || publish_resolution_sync(&path, &request)).await
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

    pub async fn get_run(
        &self,
        ingestion_run_id: &str,
        memory_space_id: &str,
    ) -> MemoryResult<IngestionRun> {
        let path = self.path.clone();
        let ingestion_run_id = ingestion_run_id.to_string();
        let memory_space_id = memory_space_id.to_string();
        run_sqlite_operation(move || get_run_sync(&path, &ingestion_run_id, &memory_space_id)).await
    }

    pub async fn get_graph_memory_record(
        &self,
        memory_record_id: &str,
        memory_space_id: &str,
    ) -> MemoryResult<GraphMemoryRecord> {
        let path = self.path.clone();
        let memory_record_id = memory_record_id.to_string();
        let memory_space_id = memory_space_id.to_string();
        run_sqlite_operation(move || {
            get_graph_memory_record_sync(&path, &memory_record_id, &memory_space_id)
        })
        .await
    }

    pub async fn get_extraction_run(
        &self,
        extraction_run_id: &str,
        memory_space_id: &str,
    ) -> MemoryResult<ExtractionRun> {
        let path = self.path.clone();
        let extraction_run_id = extraction_run_id.to_string();
        let memory_space_id = memory_space_id.to_string();
        run_sqlite_operation(move || {
            get_extraction_run_sync(&path, &extraction_run_id, &memory_space_id)
        })
        .await
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

fn claim_extraction_run_sync(
    path: &Path,
    ingestion_run_id: &str,
) -> MemoryResult<ClaimedExtractionRun> {
    let mut connection = open_graph_connection(path)?;
    let transaction = connection.transaction()?;
    let (memory_space_id, memory_record_id, text, metadata_json, status, stage, attempt_count) =
        transaction.query_row(
            "SELECT runs.memory_space_id,
                runs.memory_record_id,
                records.text,
                records.metadata_json,
                runs.status,
                runs.stage,
                runs.attempt_count
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
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )?;

    if status != "running" || stage != "extraction" {
        return Err(MemoryError::InvalidInput {
            message: format!("ingestion run is not ready for extraction: {status}/{stage}"),
        });
    }

    let now = current_time_ms() as i64;
    let updated = transaction.execute(
        "UPDATE graph_ingestion_runs
         SET stage = 'extracting',
             updated_at_ms = ?1
         WHERE id = ?2
           AND status = 'running'
           AND stage = 'extraction'
           AND attempt_count = ?3",
        params![now, ingestion_run_id, attempt_count],
    )?;
    if updated != 1 {
        return Err(MemoryError::StoreBackend {
            message: "failed to claim extraction run".to_string(),
        });
    }

    let extraction_attempt_number: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(attempt_number), 0) + 1
         FROM graph_extraction_runs
         WHERE ingestion_run_id = ?1
           AND memory_space_id = ?2",
        params![ingestion_run_id, &memory_space_id],
        |row| row.get(0),
    )?;
    let metadata = serde_json::from_str(&metadata_json)?;
    // Extraction only uses the current record as context. Cross-record extraction context
    // requires a separate retrieval/selection step and is intentionally not done here.
    let context_record_ids = vec![memory_record_id.clone()];

    transaction.commit()?;
    Ok(ClaimedExtractionRun {
        ingestion_run_id: ingestion_run_id.to_string(),
        memory_space_id,
        memory_record_id,
        text,
        metadata,
        attempt_count,
        extraction_attempt_number,
        context_record_ids,
    })
}

fn store_extraction_success_sync(
    path: &Path,
    completion: &ExtractionRunCompletion,
) -> MemoryResult<ExtractionRun> {
    let mut connection = open_graph_connection(path)?;
    let transaction = connection.transaction()?;
    let now = current_time_ms() as i64;
    let extraction_run_id = Uuid::new_v4().to_string();
    let context_record_ids_json = serde_json::to_string(&completion.context_record_ids)?;
    let structured_output_json = serde_json::to_string(&completion.structured_output)?;

    transaction.execute(
        "INSERT INTO graph_extraction_runs (
            id, memory_space_id, ingestion_run_id, attempt_number, status,
            extractor_name, model, prompt_version, schema_version, type_registry_version,
            context_record_ids_json, structured_output_json, input_tokens, output_tokens,
            latency_ms, created_at_ms, completed_at_ms
         ) VALUES (?1, ?2, ?3, ?4, 'completed', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15)",
        params![
            &extraction_run_id,
            &completion.memory_space_id,
            &completion.ingestion_run_id,
            completion.attempt_number,
            &completion.extractor_name,
            &completion.model,
            &completion.prompt_version,
            &completion.schema_version,
            &completion.type_registry_version,
            &context_record_ids_json,
            &structured_output_json,
            completion.input_tokens,
            completion.output_tokens,
            completion.latency_ms,
            now,
        ],
    )?;

    let updated = transaction.execute(
        "UPDATE graph_ingestion_runs
         SET stage = 'resolution',
             updated_at_ms = ?1
         WHERE id = ?2
           AND memory_space_id = ?3
           AND status = 'running'
           AND stage = 'extracting'
           AND attempt_count = ?4",
        params![
            now,
            &completion.ingestion_run_id,
            &completion.memory_space_id,
            completion.attempt_count
        ],
    )?;
    if updated != 1 {
        return Err(MemoryError::StoreBackend {
            message: "ingestion run is no longer current for extraction completion".to_string(),
        });
    }

    transaction.commit()?;
    Ok(ExtractionRun {
        id: extraction_run_id,
        memory_space_id: completion.memory_space_id.clone(),
        ingestion_run_id: completion.ingestion_run_id.clone(),
        attempt_number: completion.attempt_number,
        status: "completed".to_string(),
        extractor_name: completion.extractor_name.clone(),
        model: completion.model.clone(),
        prompt_version: completion.prompt_version.clone(),
        schema_version: completion.schema_version.clone(),
        type_registry_version: completion.type_registry_version.clone(),
        context_record_ids: completion.context_record_ids.clone(),
        structured_output: Some(completion.structured_output.clone()),
        input_tokens: completion.input_tokens,
        output_tokens: completion.output_tokens,
        latency_ms: completion.latency_ms,
        error_code: None,
        error_message: None,
        created_at_ms: now as u64,
        completed_at_ms: Some(now as u64),
    })
}

fn store_extraction_failure_sync(
    path: &Path,
    failure: &ExtractionRunFailure,
) -> MemoryResult<ExtractionRun> {
    let mut connection = open_graph_connection(path)?;
    let transaction = connection.transaction()?;
    let now = current_time_ms() as i64;
    let extraction_run_id = Uuid::new_v4().to_string();
    let context_record_ids_json = serde_json::to_string(&failure.context_record_ids)?;

    transaction.execute(
        "INSERT INTO graph_extraction_runs (
            id, memory_space_id, ingestion_run_id, attempt_number, status,
            extractor_name, model, prompt_version, schema_version, type_registry_version,
            context_record_ids_json, latency_ms, error_code, error_message,
            created_at_ms, completed_at_ms
         ) VALUES (?1, ?2, ?3, ?4, 'failed', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)",
        params![
            &extraction_run_id,
            &failure.memory_space_id,
            &failure.ingestion_run_id,
            failure.attempt_number,
            &failure.extractor_name,
            &failure.model,
            &failure.prompt_version,
            &failure.schema_version,
            &failure.type_registry_version,
            &context_record_ids_json,
            failure.latency_ms,
            &failure.error_code,
            &failure.error_message,
            now,
        ],
    )?;

    let updated = transaction.execute(
        "UPDATE graph_ingestion_runs
         SET status = 'failed',
             error_code = ?1,
             error_message = ?2,
             updated_at_ms = ?3,
             completed_at_ms = ?3
         WHERE id = ?4
           AND memory_space_id = ?5
           AND status = 'running'
           AND stage = 'extracting'
           AND attempt_count = ?6",
        params![
            &failure.error_code,
            &failure.error_message,
            now,
            &failure.ingestion_run_id,
            &failure.memory_space_id,
            failure.attempt_count
        ],
    )?;
    if updated != 1 {
        return Err(MemoryError::StoreBackend {
            message: "ingestion run is no longer current for extraction failure".to_string(),
        });
    }

    transaction.commit()?;
    Ok(ExtractionRun {
        id: extraction_run_id,
        memory_space_id: failure.memory_space_id.clone(),
        ingestion_run_id: failure.ingestion_run_id.clone(),
        attempt_number: failure.attempt_number,
        status: "failed".to_string(),
        extractor_name: failure.extractor_name.clone(),
        model: failure.model.clone(),
        prompt_version: failure.prompt_version.clone(),
        schema_version: failure.schema_version.clone(),
        type_registry_version: failure.type_registry_version.clone(),
        context_record_ids: failure.context_record_ids.clone(),
        structured_output: None,
        input_tokens: None,
        output_tokens: None,
        latency_ms: failure.latency_ms,
        error_code: Some(failure.error_code.clone()),
        error_message: Some(failure.error_message.clone()),
        created_at_ms: now as u64,
        completed_at_ms: Some(now as u64),
    })
}

fn claim_resolution_run_sync(
    path: &Path,
    ingestion_run_id: &str,
) -> MemoryResult<ClaimedResolutionRun> {
    let mut connection = open_graph_connection(path)?;
    let transaction = connection.transaction()?;
    let (
        memory_space_id,
        memory_record_id,
        text,
        status,
        stage,
        attempt_count,
        extraction_run_id,
        structured_output_json,
    ) = transaction.query_row(
        "SELECT runs.memory_space_id,
                runs.memory_record_id,
                records.text,
                runs.status,
                runs.stage,
                runs.attempt_count,
                extraction.id,
                extraction.structured_output_json
         FROM graph_ingestion_runs runs
         JOIN graph_memory_records records
           ON records.id = runs.memory_record_id
          AND records.memory_space_id = runs.memory_space_id
         JOIN graph_extraction_runs extraction
           ON extraction.ingestion_run_id = runs.id
          AND extraction.memory_space_id = runs.memory_space_id
         WHERE runs.id = ?1
           AND extraction.status = 'completed'
         ORDER BY extraction.attempt_number DESC
         LIMIT 1",
        params![ingestion_run_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        },
    )?;

    if status != "running" || stage != "resolution" {
        return Err(MemoryError::InvalidInput {
            message: format!("ingestion run is not ready for resolution: {status}/{stage}"),
        });
    }

    let structured_output_json =
        structured_output_json.ok_or_else(|| MemoryError::StoreBackend {
            message: "completed extraction run has no structured output".to_string(),
        })?;
    let extraction_output = serde_json::from_str(&structured_output_json)?;
    let now = current_time_ms() as i64;
    let updated = transaction.execute(
        "UPDATE graph_ingestion_runs
         SET stage = 'resolving',
             updated_at_ms = ?1
         WHERE id = ?2
           AND memory_space_id = ?3
           AND status = 'running'
           AND stage = 'resolution'
           AND attempt_count = ?4",
        params![now, ingestion_run_id, &memory_space_id, attempt_count],
    )?;
    if updated != 1 {
        return Err(MemoryError::StoreBackend {
            message: "failed to claim resolution run".to_string(),
        });
    }

    transaction.commit()?;
    Ok(ClaimedResolutionRun {
        ingestion_run_id: ingestion_run_id.to_string(),
        memory_space_id,
        memory_record_id,
        text,
        attempt_count,
        extraction_run_id,
        extraction_output,
    })
}

fn publish_resolution_sync(
    path: &Path,
    request: &ResolutionPublishRequest,
) -> MemoryResult<ResolutionPublishResult> {
    let mut connection = open_graph_connection(path)?;
    let transaction = connection.transaction()?;
    let now = current_time_ms() as i64;
    let mut entity_map = HashMap::new();
    let mut entities_created = 0;
    let mut entities_reused = 0;
    let mut facts_created = 0;
    let mut facts_reused = 0;
    let mut evidence_inserted = 0;

    for entity in &request.extraction_output.entities {
        let resolved = resolve_entity_candidate(&transaction, request, entity, now)?;
        if resolved.created {
            entities_created += 1;
        } else {
            entities_reused += 1;
        }
        entity_map.insert(entity.local_id.clone(), resolved.entity_id);
    }

    for fact in &request.extraction_output.facts {
        let subject_entity_id =
            entity_map
                .get(&fact.subject_ref)
                .ok_or_else(|| MemoryError::StoreBackend {
                    message: format!(
                        "resolved subject entity missing for fact '{}'",
                        fact.local_id
                    ),
                })?;
        let object_entity_id =
            entity_map
                .get(&fact.object_ref)
                .ok_or_else(|| MemoryError::StoreBackend {
                    message: format!(
                        "resolved object entity missing for fact '{}'",
                        fact.local_id
                    ),
                })?;
        let resolved = resolve_fact_candidate(
            &transaction,
            request,
            fact,
            subject_entity_id,
            object_entity_id,
            now,
        )?;
        if resolved.created {
            facts_created += 1;
        } else {
            facts_reused += 1;
        }
        evidence_inserted +=
            insert_fact_evidence_group(&transaction, request, &resolved.fact_id, fact, now)?;
    }

    let updated = transaction.execute(
        "UPDATE graph_ingestion_runs
         SET status = 'completed',
             stage = 'completed',
             updated_at_ms = ?1,
             completed_at_ms = ?1
         WHERE id = ?2
           AND memory_space_id = ?3
           AND status = 'running'
           AND stage = 'resolving'
           AND attempt_count = ?4",
        params![
            now,
            &request.ingestion_run_id,
            &request.memory_space_id,
            request.attempt_count
        ],
    )?;
    if updated != 1 {
        return Err(MemoryError::StoreBackend {
            message: "ingestion run is no longer current for resolution completion".to_string(),
        });
    }

    let ingestion_run = get_run_in_transaction(
        &transaction,
        &request.ingestion_run_id,
        &request.memory_space_id,
    )?;
    transaction.commit()?;
    Ok(ResolutionPublishResult {
        ingestion_run,
        entities_created,
        entities_reused,
        facts_created,
        facts_reused,
        evidence_inserted,
    })
}

fn get_run_in_transaction(
    transaction: &Transaction<'_>,
    ingestion_run_id: &str,
    memory_space_id: &str,
) -> MemoryResult<IngestionRun> {
    transaction
        .query_row(
            "SELECT id, memory_space_id, memory_record_id, idempotency_key, input_hash,
                    status, stage, attempt_count, pipeline_version, error_code, error_message,
                    created_at_ms, started_at_ms, updated_at_ms, completed_at_ms
             FROM graph_ingestion_runs
             WHERE id = ?1
               AND memory_space_id = ?2",
            params![ingestion_run_id, memory_space_id],
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

struct ResolvedEntity {
    entity_id: String,
    created: bool,
}

struct ResolvedFact {
    fact_id: String,
    created: bool,
}

fn resolve_entity_candidate(
    transaction: &Transaction<'_>,
    request: &ResolutionPublishRequest,
    candidate: &ExtractedEntityCandidate,
    now: i64,
) -> MemoryResult<ResolvedEntity> {
    let normalized_name = normalize_graph_text(&candidate.name);
    if let Some(entity_id) = find_entity_by_normalized_name(
        transaction,
        &request.memory_space_id,
        &candidate.entity_type,
        &normalized_name,
    )? {
        ensure_entity_alias(
            transaction,
            &request.memory_space_id,
            &entity_id,
            &candidate.name,
            &normalized_name,
            now,
        )?;
        insert_resolution_decision(
            transaction,
            request,
            ResolutionDecisionInsert {
                decision_kind: "entity_resolution",
                input_key: &candidate.local_id,
                candidate_ids: std::slice::from_ref(&entity_id),
                selected_id: Some(&entity_id),
                action: "reuse",
                now,
            },
        )?;
        return Ok(ResolvedEntity {
            entity_id,
            created: false,
        });
    }
    if let Some(entity_id) = find_unique_entity_by_alias(
        transaction,
        &request.memory_space_id,
        &candidate.entity_type,
        &normalized_name,
    )? {
        ensure_entity_alias(
            transaction,
            &request.memory_space_id,
            &entity_id,
            &candidate.name,
            &normalized_name,
            now,
        )?;
        insert_resolution_decision(
            transaction,
            request,
            ResolutionDecisionInsert {
                decision_kind: "entity_resolution",
                input_key: &candidate.local_id,
                candidate_ids: std::slice::from_ref(&entity_id),
                selected_id: Some(&entity_id),
                action: "reuse",
                now,
            },
        )?;
        return Ok(ResolvedEntity {
            entity_id,
            created: false,
        });
    }

    let entity_id = Uuid::new_v4().to_string();
    transaction.execute(
        "INSERT INTO graph_entities (
            id, memory_space_id, canonical_name, normalized_name, entity_type, status,
            type_registry_version, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?7, ?7)",
        params![
            &entity_id,
            &request.memory_space_id,
            &candidate.name,
            &normalized_name,
            &candidate.entity_type,
            &request.type_registry_version,
            now,
        ],
    )?;
    transaction.execute(
        "INSERT INTO graph_entity_fts (id, memory_space_id, canonical_name)
         VALUES (?1, ?2, ?3)",
        params![&entity_id, &request.memory_space_id, &candidate.name],
    )?;
    ensure_entity_alias(
        transaction,
        &request.memory_space_id,
        &entity_id,
        &candidate.name,
        &normalized_name,
        now,
    )?;
    insert_resolution_decision(
        transaction,
        request,
        ResolutionDecisionInsert {
            decision_kind: "entity_resolution",
            input_key: &candidate.local_id,
            candidate_ids: &[],
            selected_id: Some(&entity_id),
            action: "create",
            now,
        },
    )?;
    Ok(ResolvedEntity {
        entity_id,
        created: true,
    })
}

fn find_entity_by_normalized_name(
    transaction: &Transaction<'_>,
    memory_space_id: &str,
    entity_type: &str,
    normalized_name: &str,
) -> MemoryResult<Option<String>> {
    transaction
        .query_row(
            "SELECT id
             FROM graph_entities
             WHERE memory_space_id = ?1
               AND entity_type = ?2
               AND normalized_name = ?3
               AND status = 'active'
               AND deleted_at_ms IS NULL
             ORDER BY created_at_ms, id
             LIMIT 1",
            params![memory_space_id, entity_type, normalized_name],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

fn find_unique_entity_by_alias(
    transaction: &Transaction<'_>,
    memory_space_id: &str,
    entity_type: &str,
    normalized_alias: &str,
) -> MemoryResult<Option<String>> {
    let mut statement = transaction.prepare(
        "SELECT entities.id
         FROM graph_entity_aliases aliases
         JOIN graph_entities entities
           ON entities.id = aliases.entity_id
          AND entities.memory_space_id = aliases.memory_space_id
         WHERE aliases.memory_space_id = ?1
           AND aliases.normalized_alias = ?2
           AND aliases.deleted_at_ms IS NULL
           AND entities.entity_type = ?3
           AND entities.status = 'active'
           AND entities.deleted_at_ms IS NULL
         ORDER BY entities.created_at_ms, entities.id
         LIMIT 2",
    )?;
    let rows = statement
        .query_map(
            params![memory_space_id, normalized_alias, entity_type],
            |row| row.get::<_, String>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(if rows.len() == 1 {
        Some(rows[0].clone())
    } else {
        None
    })
}

fn ensure_entity_alias(
    transaction: &Transaction<'_>,
    memory_space_id: &str,
    entity_id: &str,
    display_alias: &str,
    normalized_alias: &str,
    now: i64,
) -> MemoryResult<()> {
    let alias_id = Uuid::new_v4().to_string();
    let inserted = transaction.execute(
        "INSERT OR IGNORE INTO graph_entity_aliases (
            id, memory_space_id, entity_id, display_alias, normalized_alias, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            &alias_id,
            memory_space_id,
            entity_id,
            display_alias,
            normalized_alias,
            now,
        ],
    )?;
    if inserted == 1 {
        transaction.execute(
            "INSERT INTO graph_entity_alias_fts (id, memory_space_id, display_alias)
             VALUES (?1, ?2, ?3)",
            params![&alias_id, memory_space_id, display_alias],
        )?;
    }
    Ok(())
}

fn resolve_fact_candidate(
    transaction: &Transaction<'_>,
    request: &ResolutionPublishRequest,
    candidate: &ExtractedFactCandidate,
    subject_entity_id: &str,
    object_entity_id: &str,
    now: i64,
) -> MemoryResult<ResolvedFact> {
    let normalized_fact_text = normalize_graph_text(&candidate.fact_text);
    let dedup_key = fact_dedup_key(
        subject_entity_id,
        &candidate.predicate,
        object_entity_id,
        &normalized_fact_text,
    );
    if let Some(fact_id) =
        find_fact_by_dedup_key(transaction, &request.memory_space_id, &dedup_key)?
    {
        insert_resolution_decision(
            transaction,
            request,
            ResolutionDecisionInsert {
                decision_kind: "fact_resolution",
                input_key: &candidate.local_id,
                candidate_ids: std::slice::from_ref(&fact_id),
                selected_id: Some(&fact_id),
                action: "reuse",
                now,
            },
        )?;
        return Ok(ResolvedFact {
            fact_id,
            created: false,
        });
    }

    let fact_id = Uuid::new_v4().to_string();
    transaction.execute(
        "INSERT INTO graph_facts (
            id, memory_space_id, subject_entity_id, predicate, object_entity_id,
            fact_text, dedup_key, status, valid_from_ms, valid_to_ms, recorded_at_ms,
            type_registry_version
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', ?8, ?9, ?10, ?11)",
        params![
            &fact_id,
            &request.memory_space_id,
            subject_entity_id,
            &candidate.predicate,
            object_entity_id,
            &candidate.fact_text,
            &dedup_key,
            candidate.valid_from_ms.map(|value| value as i64),
            candidate.valid_to_ms.map(|value| value as i64),
            now,
            &request.type_registry_version,
        ],
    )?;
    transaction.execute(
        "INSERT INTO graph_fact_fts (id, memory_space_id, fact_text)
         VALUES (?1, ?2, ?3)",
        params![&fact_id, &request.memory_space_id, &candidate.fact_text],
    )?;
    transaction.execute(
        "INSERT INTO graph_fact_status_history (
            id, memory_space_id, fact_id, old_status, new_status, reason_code,
            trigger_record_id, created_at_ms
         ) VALUES (?1, ?2, ?3, NULL, 'active', 'created_from_extraction', ?4, ?5)",
        params![
            Uuid::new_v4().to_string(),
            &request.memory_space_id,
            &fact_id,
            &request.memory_record_id,
            now,
        ],
    )?;
    insert_resolution_decision(
        transaction,
        request,
        ResolutionDecisionInsert {
            decision_kind: "fact_resolution",
            input_key: &candidate.local_id,
            candidate_ids: &[],
            selected_id: Some(&fact_id),
            action: "create",
            now,
        },
    )?;
    Ok(ResolvedFact {
        fact_id,
        created: true,
    })
}

fn find_fact_by_dedup_key(
    transaction: &Transaction<'_>,
    memory_space_id: &str,
    dedup_key: &str,
) -> MemoryResult<Option<String>> {
    transaction
        .query_row(
            "SELECT id
             FROM graph_facts
             WHERE memory_space_id = ?1
               AND dedup_key = ?2
               AND status = 'active'
               AND retired_at_ms IS NULL
             ORDER BY recorded_at_ms, id
             LIMIT 1",
            params![memory_space_id, dedup_key],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

fn insert_fact_evidence_group(
    transaction: &Transaction<'_>,
    request: &ResolutionPublishRequest,
    fact_id: &str,
    candidate: &ExtractedFactCandidate,
    now: i64,
) -> MemoryResult<usize> {
    let evidence_group_id = Uuid::new_v4().to_string();
    transaction.execute(
        "INSERT INTO graph_fact_evidence_groups (
            id, memory_space_id, fact_id, evidence_kind, extraction_run_id, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            &evidence_group_id,
            &request.memory_space_id,
            fact_id,
            FACT_EVIDENCE_KIND_SUPPORT,
            &request.extraction_run_id,
            now,
        ],
    )?;

    for evidence in &candidate.evidence {
        transaction.execute(
            "INSERT INTO graph_fact_evidence (
                id, memory_space_id, evidence_group_id, memory_record_id,
                evidence_text, start_byte, end_byte, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                Uuid::new_v4().to_string(),
                &request.memory_space_id,
                &evidence_group_id,
                &request.memory_record_id,
                evidence.text.as_deref(),
                evidence.start_byte.map(|value| value as i64),
                evidence.end_byte.map(|value| value as i64),
                now,
            ],
        )?;
    }
    Ok(candidate.evidence.len())
}

struct ResolutionDecisionInsert<'a> {
    decision_kind: &'a str,
    input_key: &'a str,
    candidate_ids: &'a [String],
    selected_id: Option<&'a str>,
    action: &'a str,
    now: i64,
}

fn insert_resolution_decision(
    transaction: &Transaction<'_>,
    request: &ResolutionPublishRequest,
    decision: ResolutionDecisionInsert<'_>,
) -> MemoryResult<()> {
    let candidate_ids_json = serde_json::to_string(decision.candidate_ids)?;
    transaction.execute(
        "INSERT INTO graph_resolution_decisions (
            id, memory_space_id, ingestion_run_id, decision_kind, input_key,
            candidate_ids_json, selected_id, action, method, resolver_version, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            Uuid::new_v4().to_string(),
            &request.memory_space_id,
            &request.ingestion_run_id,
            decision.decision_kind,
            decision.input_key,
            &candidate_ids_json,
            decision.selected_id,
            decision.action,
            RESOLUTION_METHOD_DETERMINISTIC,
            &request.resolver_version,
            decision.now,
        ],
    )?;
    Ok(())
}

fn fact_dedup_key(
    subject_entity_id: &str,
    predicate: &str,
    object_entity_id: &str,
    normalized_fact_text: &str,
) -> String {
    serde_json::to_string(&[
        subject_entity_id,
        predicate,
        object_entity_id,
        normalized_fact_text,
    ])
    .expect("fact dedup key fields must serialize to JSON")
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
    // This helper is used before a stage commits completion. The stage predicate is
    // intentionally both a guard and the value preserved on the failed run.
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

fn get_run_sync(
    path: &Path,
    ingestion_run_id: &str,
    memory_space_id: &str,
) -> MemoryResult<IngestionRun> {
    let connection = open_graph_connection(path)?;
    connection
        .query_row(
            "SELECT id, memory_space_id, memory_record_id, idempotency_key, input_hash,
                    status, stage, attempt_count, pipeline_version, error_code, error_message,
                    created_at_ms, started_at_ms, updated_at_ms, completed_at_ms
             FROM graph_ingestion_runs
             WHERE id = ?1
               AND memory_space_id = ?2",
            params![ingestion_run_id, memory_space_id],
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

fn get_extraction_run_sync(
    path: &Path,
    extraction_run_id: &str,
    memory_space_id: &str,
) -> MemoryResult<ExtractionRun> {
    let connection = open_graph_connection(path)?;
    let (
        id,
        memory_space_id,
        ingestion_run_id,
        attempt_number,
        status,
        extractor_name,
        model,
        prompt_version,
        schema_version,
        type_registry_version,
        context_record_ids_json,
        structured_output_json,
        input_tokens,
        output_tokens,
        latency_ms,
        error_code,
        error_message,
        created_at_ms,
        completed_at_ms,
    ) = connection.query_row(
        "SELECT id, memory_space_id, ingestion_run_id, attempt_number, status,
                extractor_name, model, prompt_version, schema_version, type_registry_version,
                context_record_ids_json, structured_output_json, input_tokens, output_tokens,
                latency_ms, error_code, error_message, created_at_ms, completed_at_ms
         FROM graph_extraction_runs
         WHERE id = ?1
           AND memory_space_id = ?2",
        params![extraction_run_id, memory_space_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, Option<i64>>(12)?,
                row.get::<_, Option<i64>>(13)?,
                row.get::<_, Option<i64>>(14)?,
                row.get::<_, Option<String>>(15)?,
                row.get::<_, Option<String>>(16)?,
                row.get::<_, i64>(17)?,
                row.get::<_, Option<i64>>(18)?,
            ))
        },
    )?;

    Ok(ExtractionRun {
        id,
        memory_space_id,
        ingestion_run_id,
        attempt_number,
        status,
        extractor_name,
        model,
        prompt_version,
        schema_version,
        type_registry_version,
        context_record_ids: serde_json::from_str(&context_record_ids_json)?,
        structured_output: structured_output_json
            .map(|value| serde_json::from_str(&value))
            .transpose()?,
        input_tokens,
        output_tokens,
        latency_ms,
        error_code,
        error_message,
        created_at_ms: created_at_ms as u64,
        completed_at_ms: completed_at_ms.map(|value| value as u64),
    })
}

fn get_graph_memory_record_sync(
    path: &Path,
    memory_record_id: &str,
    memory_space_id: &str,
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
         WHERE id = ?1
           AND memory_space_id = ?2",
        params![memory_record_id, memory_space_id],
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
    use super::{fact_dedup_key, open_graph_connection};
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

    #[test]
    fn fact_dedup_key_does_not_collide_when_text_contains_separator_like_control_characters() {
        let key_with_control_in_text =
            fact_dedup_key("subject", "predicate", "object", "left\u{1f}right");
        let key_with_control_in_object =
            fact_dedup_key("subject", "predicate", "object\u{1f}left", "right");

        assert_ne!(key_with_control_in_text, key_with_control_in_object);
    }
}
