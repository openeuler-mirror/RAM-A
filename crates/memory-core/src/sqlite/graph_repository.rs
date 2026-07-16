use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension, Transaction};
use uuid::Uuid;

use crate::graph::{
    normalize_graph_text, stable_input_hash, ContextBundle, Entity, ExtractedEntityCandidate,
    ExtractedFactCandidate, ExtractionRun, Fact, FactContextUnit, FactLink, FactStatus,
    GraphExtractionOutput, GraphInputHashFields, GraphMemoryRecord, IngestionRun,
};
use crate::{
    GraphAddMemoryRequest, GraphAddMemoryResponse, GraphRetrieveContextRequest, MemoryError,
    MemoryResult,
};

const GRAPH_PIPELINE_VERSION: &str = "graph-pipeline-v1";
const FACT_EVIDENCE_KIND_SUPPORT: &str = "support";
const RESOLUTION_METHOD_DETERMINISTIC: &str = "deterministic";
const MAX_GRAPH_RETRIEVAL_QUERY_BYTES: usize = 4 * 1024;
const MAX_GRAPH_FTS_QUERY_TERMS: usize = 256;

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

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum GraphSeedKind {
    Entity,
    Fact,
}

#[derive(Clone, Debug)]
struct GraphSeed {
    kind: GraphSeedKind,
    id: String,
    score: f32,
}

#[derive(Clone, Debug)]
struct FactCandidate {
    fact_id: String,
    score: f32,
    path: Vec<String>,
    recorded_at_ms: u64,
}

#[derive(Clone, Debug)]
struct BundleMembers {
    records: Vec<GraphMemoryRecord>,
    entities: Vec<Entity>,
    facts: Vec<Fact>,
    fact_links: Vec<FactLink>,
    paths: Vec<Vec<String>>,
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

    pub async fn retrieve_context(
        &self,
        request: GraphRetrieveContextRequest,
    ) -> MemoryResult<ContextBundle> {
        let path = self.path.clone();
        run_sqlite_operation(move || retrieve_context_sync(&path, &request)).await
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

fn retrieve_context_sync(
    path: &Path,
    request: &GraphRetrieveContextRequest,
) -> MemoryResult<ContextBundle> {
    let query = request.query.trim();
    if query.is_empty() {
        return Err(MemoryError::InvalidInput {
            message: "graph retrieval query must not be empty".to_string(),
        });
    }
    if query.len() > MAX_GRAPH_RETRIEVAL_QUERY_BYTES {
        return Err(MemoryError::InvalidInput {
            message: format!(
                "graph retrieval query must be at most {MAX_GRAPH_RETRIEVAL_QUERY_BYTES} bytes"
            ),
        });
    }

    let connection = open_graph_connection(path)?;
    let Some(fts_query) = graph_fts_query(query) else {
        return Ok(ContextBundle {
            query: request.query.clone(),
            memory_space_id: request.memory_space_id.clone(),
            reference_time_ms: request.reference_time_ms.unwrap_or_else(current_time_ms),
            fact_context_units: Vec::new(),
            records: Vec::new(),
            entities: Vec::new(),
            facts: Vec::new(),
            fact_links: Vec::new(),
            paths: Vec::new(),
            truncation: None,
            degraded_reason: Some("query produced no graph retrieval terms".to_string()),
        });
    };

    let seeds = collect_graph_seeds(
        &connection,
        &request.memory_space_id,
        &fts_query,
        request.seed_limit(),
    )?;
    let fact_candidates =
        expand_seed_facts(&connection, &request.memory_space_id, &seeds, request.top_k)?;
    let (fact_context_units, loaded_facts) =
        load_fact_context_units(&connection, request, &fact_candidates)?;
    let bundle_members = collect_bundle_members(&fact_context_units, loaded_facts);

    Ok(ContextBundle {
        query: request.query.clone(),
        memory_space_id: request.memory_space_id.clone(),
        reference_time_ms: request.reference_time_ms.unwrap_or_else(current_time_ms),
        fact_context_units,
        records: bundle_members.records,
        entities: bundle_members.entities,
        facts: bundle_members.facts,
        fact_links: bundle_members.fact_links,
        paths: bundle_members.paths,
        truncation: None,
        degraded_reason: None,
    })
}

fn graph_fts_query(query: &str) -> Option<String> {
    let terms = query
        .split_whitespace()
        .filter_map(|term| {
            let trimmed = term.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(format!("\"{}\"", trimmed.replace('"', "\"\"")))
            }
        })
        .take(MAX_GRAPH_FTS_QUERY_TERMS)
        .collect::<Vec<_>>();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" OR "))
    }
}

fn collect_graph_seeds(
    connection: &Connection,
    memory_space_id: &str,
    fts_query: &str,
    limit: usize,
) -> MemoryResult<Vec<GraphSeed>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let mut seeds = Vec::new();
    collect_entity_name_seeds(connection, memory_space_id, fts_query, limit, &mut seeds)?;
    collect_entity_alias_seeds(connection, memory_space_id, fts_query, limit, &mut seeds)?;
    collect_fact_text_seeds(connection, memory_space_id, fts_query, limit, &mut seeds)?;
    collect_record_evidence_fact_seeds(connection, memory_space_id, fts_query, limit, &mut seeds)?;

    let mut best_by_key: HashMap<(GraphSeedKind, String), f32> = HashMap::new();
    for seed in seeds {
        let key = (seed.kind, seed.id);
        best_by_key
            .entry(key)
            .and_modify(|score| *score = score.max(seed.score))
            .or_insert(seed.score);
    }

    let mut merged = best_by_key
        .into_iter()
        .map(|((kind, id), score)| GraphSeed { kind, id, score })
        .collect::<Vec<_>>();
    merged.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.id.cmp(&right.id))
    });
    merged.truncate(limit);
    Ok(merged)
}

fn collect_entity_name_seeds(
    connection: &Connection,
    memory_space_id: &str,
    fts_query: &str,
    limit: usize,
    seeds: &mut Vec<GraphSeed>,
) -> MemoryResult<()> {
    let mut statement = connection.prepare(
        "SELECT entities.id
         FROM graph_entity_fts
         JOIN graph_entities entities
           ON entities.id = graph_entity_fts.id
          AND entities.memory_space_id = graph_entity_fts.memory_space_id
         WHERE graph_entity_fts MATCH ?1
           AND graph_entity_fts.memory_space_id = ?2
           AND entities.status = 'active'
           AND entities.deleted_at_ms IS NULL
         ORDER BY bm25(graph_entity_fts), entities.created_at_ms, entities.id
         LIMIT ?3",
    )?;
    collect_seed_rows(
        &mut statement,
        params![fts_query, memory_space_id, limit as i64],
        GraphSeedKind::Entity,
        seeds,
    )
}

fn collect_entity_alias_seeds(
    connection: &Connection,
    memory_space_id: &str,
    fts_query: &str,
    limit: usize,
    seeds: &mut Vec<GraphSeed>,
) -> MemoryResult<()> {
    let mut statement = connection.prepare(
        "SELECT entities.id
         FROM graph_entity_alias_fts
         JOIN graph_entity_aliases aliases
           ON aliases.id = graph_entity_alias_fts.id
          AND aliases.memory_space_id = graph_entity_alias_fts.memory_space_id
         JOIN graph_entities entities
           ON entities.id = aliases.entity_id
          AND entities.memory_space_id = aliases.memory_space_id
         WHERE graph_entity_alias_fts MATCH ?1
           AND graph_entity_alias_fts.memory_space_id = ?2
           AND aliases.deleted_at_ms IS NULL
           AND entities.status = 'active'
           AND entities.deleted_at_ms IS NULL
         GROUP BY entities.id
         ORDER BY MIN(aliases.created_at_ms), entities.id
         LIMIT ?3",
    )?;
    collect_seed_rows(
        &mut statement,
        params![fts_query, memory_space_id, limit as i64],
        GraphSeedKind::Entity,
        seeds,
    )
}

fn collect_fact_text_seeds(
    connection: &Connection,
    memory_space_id: &str,
    fts_query: &str,
    limit: usize,
    seeds: &mut Vec<GraphSeed>,
) -> MemoryResult<()> {
    let mut statement = connection.prepare(
        "SELECT facts.id
         FROM graph_fact_fts
         JOIN graph_facts facts
           ON facts.id = graph_fact_fts.id
          AND facts.memory_space_id = graph_fact_fts.memory_space_id
         WHERE graph_fact_fts MATCH ?1
           AND graph_fact_fts.memory_space_id = ?2
           AND facts.status = 'active'
           AND facts.retired_at_ms IS NULL
         ORDER BY bm25(graph_fact_fts), facts.recorded_at_ms, facts.id
         LIMIT ?3",
    )?;
    collect_seed_rows(
        &mut statement,
        params![fts_query, memory_space_id, limit as i64],
        GraphSeedKind::Fact,
        seeds,
    )
}

fn collect_record_evidence_fact_seeds(
    connection: &Connection,
    memory_space_id: &str,
    fts_query: &str,
    limit: usize,
    seeds: &mut Vec<GraphSeed>,
) -> MemoryResult<()> {
    let mut statement = connection.prepare(
        "SELECT facts.id
         FROM graph_memory_record_fts
         JOIN graph_memory_records records
           ON records.id = graph_memory_record_fts.id
          AND records.memory_space_id = graph_memory_record_fts.memory_space_id
         JOIN graph_fact_evidence evidence
           ON evidence.memory_record_id = records.id
          AND evidence.memory_space_id = records.memory_space_id
          AND evidence.deleted_at_ms IS NULL
         JOIN graph_fact_evidence_groups groups
           ON groups.id = evidence.evidence_group_id
          AND groups.memory_space_id = evidence.memory_space_id
          AND groups.deleted_at_ms IS NULL
         JOIN graph_facts facts
           ON facts.id = groups.fact_id
          AND facts.memory_space_id = groups.memory_space_id
         WHERE graph_memory_record_fts MATCH ?1
           AND graph_memory_record_fts.memory_space_id = ?2
           AND records.deleted_at_ms IS NULL
           AND facts.status = 'active'
           AND facts.retired_at_ms IS NULL
         GROUP BY facts.id
         ORDER BY MIN(facts.recorded_at_ms), facts.id
         LIMIT ?3",
    )?;
    collect_seed_rows(
        &mut statement,
        params![fts_query, memory_space_id, limit as i64],
        GraphSeedKind::Fact,
        seeds,
    )
}

fn collect_seed_rows<P>(
    statement: &mut rusqlite::Statement<'_>,
    params: P,
    kind: GraphSeedKind,
    seeds: &mut Vec<GraphSeed>,
) -> MemoryResult<()>
where
    P: rusqlite::Params,
{
    let rows = statement.query_map(params, |row| row.get::<_, String>(0))?;
    let mut raw_rows = Vec::new();
    for row in rows {
        raw_rows.push(row?);
    }
    append_ranked_seeds(kind, raw_rows, seeds);
    Ok(())
}

fn append_ranked_seeds(kind: GraphSeedKind, raw_rows: Vec<String>, seeds: &mut Vec<GraphSeed>) {
    for (index, id) in raw_rows.into_iter().enumerate() {
        seeds.push(GraphSeed {
            kind: kind.clone(),
            id,
            score: 1.0 / (index as f32 + 1.0),
        });
    }
}

fn expand_seed_facts(
    connection: &Connection,
    memory_space_id: &str,
    seeds: &[GraphSeed],
    limit: usize,
) -> MemoryResult<Vec<FactCandidate>> {
    if limit == 0 || seeds.is_empty() {
        return Ok(Vec::new());
    }

    let mut candidates_by_fact: HashMap<String, FactCandidate> = HashMap::new();
    for seed in seeds {
        match seed.kind {
            GraphSeedKind::Fact => {
                if let Some(recorded_at_ms) =
                    active_fact_recorded_at(connection, memory_space_id, &seed.id)?
                {
                    upsert_fact_candidate(
                        &mut candidates_by_fact,
                        FactCandidate {
                            fact_id: seed.id.clone(),
                            score: seed.score,
                            path: vec![format!("fact:{}", seed.id)],
                            recorded_at_ms,
                        },
                    );
                }
            }
            GraphSeedKind::Entity => {
                for (fact_id, recorded_at_ms) in
                    active_facts_for_entity(connection, memory_space_id, &seed.id, limit)?
                {
                    upsert_fact_candidate(
                        &mut candidates_by_fact,
                        FactCandidate {
                            fact_id: fact_id.clone(),
                            score: seed.score,
                            path: vec![format!("entity:{}", seed.id), format!("fact:{fact_id}")],
                            recorded_at_ms,
                        },
                    );
                }
            }
        }
    }

    let mut candidates = candidates_by_fact.into_values().collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.recorded_at_ms.cmp(&right.recorded_at_ms))
            .then_with(|| left.fact_id.cmp(&right.fact_id))
    });
    candidates.truncate(limit);
    Ok(candidates)
}

fn upsert_fact_candidate(
    candidates_by_fact: &mut HashMap<String, FactCandidate>,
    candidate: FactCandidate,
) {
    candidates_by_fact
        .entry(candidate.fact_id.clone())
        .and_modify(|existing| {
            if is_better_fact_candidate(&candidate, existing) {
                *existing = candidate.clone();
            }
        })
        .or_insert(candidate);
}

fn is_better_fact_candidate(candidate: &FactCandidate, existing: &FactCandidate) -> bool {
    if candidate.score > existing.score {
        return true;
    }
    if (candidate.score - existing.score).abs() > f32::EPSILON {
        return false;
    }
    let candidate_rank = fact_candidate_path_rank(&candidate.path);
    let existing_rank = fact_candidate_path_rank(&existing.path);
    candidate_rank < existing_rank
        || (candidate_rank == existing_rank && candidate.path < existing.path)
}

fn fact_candidate_path_rank(path: &[String]) -> usize {
    match path.first() {
        Some(first) if first.starts_with("fact:") => 0,
        Some(first) if first.starts_with("entity:") => 1,
        _ => 2,
    }
}

fn active_fact_recorded_at(
    connection: &Connection,
    memory_space_id: &str,
    fact_id: &str,
) -> MemoryResult<Option<u64>> {
    connection
        .query_row(
            "SELECT recorded_at_ms
             FROM graph_facts
             WHERE id = ?1
               AND memory_space_id = ?2
               AND status = 'active'
               AND retired_at_ms IS NULL",
            params![fact_id, memory_space_id],
            |row| Ok(row.get::<_, i64>(0)? as u64),
        )
        .optional()
        .map_err(Into::into)
}

fn active_facts_for_entity(
    connection: &Connection,
    memory_space_id: &str,
    entity_id: &str,
    limit: usize,
) -> MemoryResult<Vec<(String, u64)>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let mut statement = connection.prepare(
        "SELECT id, recorded_at_ms
         FROM graph_facts
         WHERE memory_space_id = ?1
           AND (subject_entity_id = ?2 OR object_entity_id = ?2)
           AND status = 'active'
           AND retired_at_ms IS NULL
         ORDER BY recorded_at_ms, id
         LIMIT ?3",
    )?;
    let rows = statement.query_map(params![memory_space_id, entity_id, limit as i64], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn load_fact_context_units(
    connection: &Connection,
    request: &GraphRetrieveContextRequest,
    fact_candidates: &[FactCandidate],
) -> MemoryResult<(Vec<FactContextUnit>, Vec<Fact>)> {
    let mut units = Vec::with_capacity(fact_candidates.len());
    let mut loaded_facts = Vec::with_capacity(fact_candidates.len());
    for candidate in fact_candidates {
        let fact = load_active_fact(connection, &request.memory_space_id, &candidate.fact_id)?;
        let subject_entity = load_entity(
            connection,
            &request.memory_space_id,
            &fact.subject_entity_id,
        )?;
        let object_entity =
            load_entity(connection, &request.memory_space_id, &fact.object_entity_id)?;
        let evidence_records = load_evidence_records_for_fact(
            connection,
            &request.memory_space_id,
            &fact.id,
            request.max_evidence_records_per_fact(),
        )?;
        let mut path = candidate.path.clone();
        for record in &evidence_records {
            let record_path = format!("record:{}", record.id);
            if !path.contains(&record_path) {
                path.push(record_path);
            }
        }
        let valid_time = match (fact.valid_from_ms, fact.valid_to_ms) {
            (Some(valid_from_ms), Some(valid_to_ms)) => Some((valid_from_ms, valid_to_ms)),
            _ => None,
        };
        units.push(FactContextUnit {
            fact_id: fact.id.clone(),
            fact_text: fact.fact_text.clone(),
            subject_entity,
            object_entity,
            predicate: fact.predicate.clone(),
            evidence_records,
            fact_links: Vec::new(),
            path,
            score: candidate.score,
            status: fact.status.clone(),
            valid_time,
        });
        loaded_facts.push(fact);
    }
    Ok((units, loaded_facts))
}

fn collect_bundle_members(
    fact_context_units: &[FactContextUnit],
    loaded_facts: Vec<Fact>,
) -> BundleMembers {
    let mut records = Vec::new();
    let mut record_ids = HashSet::new();
    let mut entities = Vec::new();
    let mut entity_ids = HashSet::new();
    let mut facts = Vec::new();
    let mut fact_ids = HashSet::new();
    let mut fact_links = Vec::new();
    let mut fact_link_ids = HashSet::<String>::new();
    let mut paths = Vec::new();

    for fact in loaded_facts {
        if fact_ids.insert(fact.id.clone()) {
            facts.push(fact);
        }
    }

    for unit in fact_context_units {
        if entity_ids.insert(unit.subject_entity.id.clone()) {
            entities.push(unit.subject_entity.clone());
        }
        if entity_ids.insert(unit.object_entity.id.clone()) {
            entities.push(unit.object_entity.clone());
        }
        for record in &unit.evidence_records {
            if record_ids.insert(record.id.clone()) {
                records.push(record.clone());
            }
        }
        for fact_link in &unit.fact_links {
            if fact_link_ids.insert(fact_link.id.clone()) {
                fact_links.push(fact_link.clone());
            }
        }
        paths.push(unit.path.clone());
    }

    BundleMembers {
        records,
        entities,
        facts,
        fact_links,
        paths,
    }
}

fn load_active_fact(
    connection: &Connection,
    memory_space_id: &str,
    fact_id: &str,
) -> MemoryResult<Fact> {
    let (
        id,
        memory_space_id,
        subject_entity_id,
        predicate,
        object_entity_id,
        fact_text,
        dedup_key,
        embedding_blob,
        embedding_dims,
        embedding_model,
        embedding_version,
        status,
        valid_from_ms,
        valid_to_ms,
        recorded_at_ms,
        retired_at_ms,
        type_registry_version,
    ) = connection.query_row(
        "SELECT id, memory_space_id, subject_entity_id, predicate, object_entity_id,
                fact_text, dedup_key, embedding, embedding_dims, embedding_model,
                embedding_version, status, valid_from_ms, valid_to_ms, recorded_at_ms,
                retired_at_ms, type_registry_version
         FROM graph_facts
         WHERE id = ?1
           AND memory_space_id = ?2
           AND status = 'active'
           AND retired_at_ms IS NULL",
        params![fact_id, memory_space_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<Vec<u8>>>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, String>(11)?,
                row.get::<_, Option<i64>>(12)?,
                row.get::<_, Option<i64>>(13)?,
                row.get::<_, i64>(14)?,
                row.get::<_, Option<i64>>(15)?,
                row.get::<_, String>(16)?,
            ))
        },
    )?;

    Ok(Fact {
        id: id.clone(),
        memory_space_id,
        subject_entity_id,
        predicate,
        object_entity_id,
        fact_text,
        dedup_key,
        embedding: blob_to_embedding(embedding_blob, embedding_dims, &id)?,
        embedding_model,
        embedding_version,
        status: parse_fact_status(&status)?,
        valid_from_ms: valid_from_ms.map(|value| value as u64),
        valid_to_ms: valid_to_ms.map(|value| value as u64),
        recorded_at_ms: recorded_at_ms as u64,
        retired_at_ms: retired_at_ms.map(|value| value as u64),
        type_registry_version,
    })
}

fn load_entity(
    connection: &Connection,
    memory_space_id: &str,
    entity_id: &str,
) -> MemoryResult<Entity> {
    let (
        id,
        memory_space_id,
        canonical_name,
        normalized_name,
        entity_type,
        name_embedding_blob,
        embedding_dims,
        embedding_model,
        embedding_version,
        status,
        type_registry_version,
        created_at_ms,
        updated_at_ms,
        deleted_at_ms,
    ) = connection.query_row(
        "SELECT id, memory_space_id, canonical_name, normalized_name, entity_type,
                name_embedding, embedding_dims, embedding_model, embedding_version,
                status, type_registry_version, created_at_ms, updated_at_ms, deleted_at_ms
         FROM graph_entities
         WHERE id = ?1
           AND memory_space_id = ?2
           AND status = 'active'
           AND deleted_at_ms IS NULL",
        params![entity_id, memory_space_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<Vec<u8>>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, i64>(11)?,
                row.get::<_, i64>(12)?,
                row.get::<_, Option<i64>>(13)?,
            ))
        },
    )?;

    Ok(Entity {
        id: id.clone(),
        memory_space_id,
        canonical_name,
        normalized_name,
        entity_type,
        name_embedding: blob_to_embedding(name_embedding_blob, embedding_dims, &id)?,
        embedding_model,
        embedding_version,
        status,
        type_registry_version,
        created_at_ms: created_at_ms as u64,
        updated_at_ms: updated_at_ms as u64,
        deleted_at_ms: deleted_at_ms.map(|value| value as u64),
    })
}

fn load_evidence_records_for_fact(
    connection: &Connection,
    memory_space_id: &str,
    fact_id: &str,
    limit: usize,
) -> MemoryResult<Vec<GraphMemoryRecord>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut statement = connection.prepare(
        "SELECT DISTINCT records.id, records.memory_space_id, records.session_id,
                records.ingestion_sequence, records.session_sequence, records.text,
                records.metadata_json, records.source_kind, records.source_ref,
                records.content_role, records.created_by_agent_id, records.observed_at_ms,
                records.embedding, records.embedding_dims, records.embedding_model,
                records.embedding_version, records.created_at_ms, records.updated_at_ms,
                records.deleted_at_ms
         FROM graph_fact_evidence_groups groups
         JOIN graph_fact_evidence evidence
           ON evidence.evidence_group_id = groups.id
          AND evidence.memory_space_id = groups.memory_space_id
          AND evidence.deleted_at_ms IS NULL
         JOIN graph_memory_records records
           ON records.id = evidence.memory_record_id
          AND records.memory_space_id = evidence.memory_space_id
         WHERE groups.fact_id = ?1
           AND groups.memory_space_id = ?2
           AND groups.deleted_at_ms IS NULL
           AND records.deleted_at_ms IS NULL
         ORDER BY groups.created_at_ms, evidence.created_at_ms, records.id
         LIMIT ?3",
    )?;
    let rows = statement.query_map(params![fact_id, memory_space_id, limit as i64], |row| {
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
    })?;

    let mut records = Vec::new();
    for row in rows {
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
        ) = row?;
        records.push(GraphMemoryRecord {
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
        });
    }
    Ok(records)
}

fn parse_fact_status(status: &str) -> MemoryResult<FactStatus> {
    match status {
        "active" => Ok(FactStatus::Active),
        "superseded" => Ok(FactStatus::Superseded),
        "retracted" => Ok(FactStatus::Retracted),
        "unsupported" => Ok(FactStatus::Unsupported),
        _ => Err(MemoryError::StoreBackend {
            message: format!("unknown graph fact status '{status}'"),
        }),
    }
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
    use super::{
        active_facts_for_entity, collect_graph_seeds, fact_dedup_key, graph_fts_query,
        open_graph_connection,
    };
    use rusqlite::params;
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

    #[test]
    fn graph_fts_query_quotes_terms_and_drops_blank_input() {
        assert_eq!(super::graph_fts_query("   "), None);
        assert_eq!(
            super::graph_fts_query("Alice Shanghai").as_deref(),
            Some("\"Alice\" OR \"Shanghai\"")
        );
        assert_eq!(
            super::graph_fts_query("Alice \"quoted\"").as_deref(),
            Some("\"Alice\" OR \"\"\"quoted\"\"\"")
        );
    }

    #[test]
    fn graph_fts_query_limits_terms() {
        let query = std::iter::repeat_n("Alice", super::MAX_GRAPH_FTS_QUERY_TERMS + 1)
            .collect::<Vec<_>>()
            .join(" ");
        let fts_query = super::graph_fts_query(&query).unwrap();

        assert_eq!(
            fts_query.matches("\"Alice\"").count(),
            super::MAX_GRAPH_FTS_QUERY_TERMS
        );
    }

    #[test]
    fn retrieve_context_rejects_oversized_query() {
        let query = "a".repeat(super::MAX_GRAPH_RETRIEVAL_QUERY_BYTES + 1);
        let error = super::retrieve_context_sync(
            Path::new(":memory:"),
            &crate::GraphRetrieveContextRequest {
                memory_space_id: "space-1".to_string(),
                query,
                top_k: 5,
                reference_time_ms: None,
                seed_limit: None,
                max_evidence_records_per_fact: None,
            },
        )
        .expect_err("oversized graph retrieval query should fail");

        assert!(format!("{error}").contains("graph retrieval query must be at most"));
    }

    #[test]
    fn alias_seed_limit_applies_after_unique_entity_deduplication() {
        let connection = open_graph_connection(Path::new(":memory:")).unwrap();
        seed_graph_space(&connection);
        insert_test_entity(&connection, "entity-a", "Person A", "person-a");
        insert_test_entity(&connection, "entity-b", "Person B", "person-b");
        insert_test_alias(&connection, "alias-a-1", "entity-a", "Alice first", 1);
        insert_test_alias(&connection, "alias-a-2", "entity-a", "Alice second", 2);
        insert_test_alias(&connection, "alias-b-1", "entity-b", "Alice third", 3);

        let fts_query = graph_fts_query("Alice").unwrap();
        let seeds = collect_graph_seeds(&connection, "space-1", &fts_query, 2).unwrap();
        let seed_ids = seeds
            .into_iter()
            .map(|seed| seed.id)
            .collect::<std::collections::HashSet<_>>();

        assert_eq!(seed_ids.len(), 2);
        assert!(seed_ids.contains("entity-a"));
        assert!(seed_ids.contains("entity-b"));
    }

    #[test]
    fn active_facts_for_entity_honors_limit() {
        let connection = open_graph_connection(Path::new(":memory:")).unwrap();
        seed_graph_space(&connection);
        insert_test_entity(&connection, "entity-a", "Person A", "person-a");
        insert_test_entity(&connection, "entity-b", "Person B", "person-b");
        for index in 0..5 {
            insert_test_fact(
                &connection,
                &format!("fact-{index}"),
                "entity-a",
                "entity-b",
                index + 1,
            );
        }

        let facts = active_facts_for_entity(&connection, "space-1", "entity-a", 2).unwrap();

        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].0, "fact-0");
        assert_eq!(facts[1].0, "fact-1");
    }

    fn seed_graph_space(connection: &rusqlite::Connection) {
        connection
            .execute(
                "INSERT INTO graph_memory_spaces (id, owner_id, status, created_at_ms)
                 VALUES ('space-1', 'owner-1', 'active', 1)",
                [],
            )
            .unwrap();
    }

    fn insert_test_entity(
        connection: &rusqlite::Connection,
        id: &str,
        canonical_name: &str,
        normalized_name: &str,
    ) {
        connection
            .execute(
                "INSERT INTO graph_entities (
                    id, memory_space_id, canonical_name, normalized_name, entity_type,
                    status, type_registry_version, created_at_ms, updated_at_ms
                 )
                 VALUES (?1, 'space-1', ?2, ?3, 'PERSON', 'active', 'registry-v1', 1, 1)",
                params![id, canonical_name, normalized_name],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO graph_entity_fts (id, memory_space_id, canonical_name)
                 VALUES (?1, 'space-1', ?2)",
                params![id, canonical_name],
            )
            .unwrap();
    }

    fn insert_test_alias(
        connection: &rusqlite::Connection,
        id: &str,
        entity_id: &str,
        display_alias: &str,
        created_at_ms: i64,
    ) {
        connection
            .execute(
                "INSERT INTO graph_entity_aliases (
                    id, memory_space_id, entity_id, display_alias, normalized_alias, created_at_ms
                 )
                 VALUES (?1, 'space-1', ?2, ?3, ?3, ?4)",
                params![id, entity_id, display_alias, created_at_ms],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO graph_entity_alias_fts (id, memory_space_id, display_alias)
                 VALUES (?1, 'space-1', ?2)",
                params![id, display_alias],
            )
            .unwrap();
    }

    fn insert_test_fact(
        connection: &rusqlite::Connection,
        id: &str,
        subject_entity_id: &str,
        object_entity_id: &str,
        recorded_at_ms: i64,
    ) {
        connection
            .execute(
                "INSERT INTO graph_facts (
                    id, memory_space_id, subject_entity_id, predicate, object_entity_id,
                    fact_text, status, recorded_at_ms, type_registry_version
                 )
                 VALUES (?1, 'space-1', ?2, 'RELATED_TO', ?3, ?1, 'active', ?4, 'registry-v1')",
                params![id, subject_entity_id, object_entity_id, recorded_at_ms],
            )
            .unwrap();
    }
}
