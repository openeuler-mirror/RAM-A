use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex as StdMutex, Weak};

use chrono::{DateTime, NaiveDate, Utc};
use memory_core::{
    AddMemoryRequest, GraphAddMemoryRequest, GraphBuildPipeline, LongTermMemory, MemoryManager,
    MemoryRecord, SearchMemoryRequest as CoreSearchRequest,
};
use memory_pipeline::extraction::MemoryExtractor;
use memory_pipeline::grounding::GroundingVerifier;
use memory_pipeline::pipeline::{run_memory_pipeline, PipelineConfig};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinSet;
use uuid::Uuid;

use crate::idempotency::{IdempotencyEntry, IdempotencyError, IdempotencyRepository, Reservation};
use crate::{IngestMessage, IngestRequest, Principal, SearchRequest};

const SOURCE_ID_VERSION: &[u8] = b"ram-a-source-v1\0";
const SESSION_ID_VERSION: &[u8] = b"ram-a-session-v1\0";
const CONTENT_HASH_VERSION: &[u8] = b"ram-a-ingest-content-v1\0";
const LOCK_KEY_VERSION: &[u8] = b"ram-a-ingest-lock-v1\0";
const MAX_SEARCH_CANDIDATES: usize = 500;

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct IngestResponse {
    pub pipeline_run_id: String,
    pub accepted_count: usize,
    pub rejected_count: usize,
    pub quarantined_count: usize,
    pub memory_ids: Vec<String>,
    pub idempotency_hit: bool,
    pub retriable: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct SearchResponse {
    pub memories: Vec<SearchResult>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct SearchResult {
    pub id: String,
    pub text: String,
    pub memory_type: String,
    pub modality: String,
    pub event_time: Value,
    pub observed_at: String,
    pub evidence_refs: Value,
    pub source_agent_id: String,
    pub graph_facts: Value,
    pub graph_facts_truncated: bool,
    pub score: f32,
}

#[derive(Clone)]
struct GraphMemoryRuntime {
    pipeline: Arc<GraphBuildPipeline>,
    build_concurrency: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceError {
    InvalidRequest,
    IdempotencyConflict,
    Pipeline,
    Storage,
}

impl ServiceError {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "INVALID_REQUEST",
            Self::IdempotencyConflict => "IDEMPOTENCY_CONFLICT",
            Self::Pipeline => "PIPELINE_FAILED",
            Self::Storage => "STORAGE_FAILED",
        }
    }

    pub fn retriable(self) -> bool {
        matches!(self, Self::Pipeline | Self::Storage)
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidRequest => "memory request is invalid",
            Self::IdempotencyConflict => "idempotency key conflicts with an earlier request",
            Self::Pipeline => "memory pipeline failed",
            Self::Storage => "memory storage failed",
        })
    }
}

impl std::error::Error for ServiceError {}

pub struct MemoryService<E: ?Sized, V: ?Sized> {
    manager: Arc<MemoryManager>,
    idempotency: IdempotencyRepository,
    extractor: Arc<E>,
    verifier: Arc<V>,
    pipeline_config: PipelineConfig,
    graph_memory: Option<GraphMemoryRuntime>,
    ingest_locks: Arc<StdMutex<HashMap<String, Weak<AsyncMutex<()>>>>>,
}

impl<E: ?Sized, V: ?Sized> Clone for MemoryService<E, V> {
    fn clone(&self) -> Self {
        Self {
            manager: self.manager.clone(),
            idempotency: self.idempotency.clone(),
            extractor: self.extractor.clone(),
            verifier: self.verifier.clone(),
            pipeline_config: self.pipeline_config.clone(),
            graph_memory: self.graph_memory.clone(),
            ingest_locks: self.ingest_locks.clone(),
        }
    }
}

impl<E, V> MemoryService<E, V>
where
    E: MemoryExtractor + ?Sized,
    V: GroundingVerifier + ?Sized,
{
    pub fn new(
        manager: Arc<MemoryManager>,
        idempotency: IdempotencyRepository,
        extractor: Arc<E>,
        verifier: Arc<V>,
    ) -> Self {
        Self::with_pipeline_config(
            manager,
            idempotency,
            extractor,
            verifier,
            PipelineConfig::default(),
        )
    }

    pub fn with_pipeline_config(
        manager: Arc<MemoryManager>,
        idempotency: IdempotencyRepository,
        extractor: Arc<E>,
        verifier: Arc<V>,
        pipeline_config: PipelineConfig,
    ) -> Self {
        Self {
            manager,
            idempotency,
            extractor,
            verifier,
            pipeline_config,
            graph_memory: None,
            ingest_locks: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    pub fn with_graph_memory(
        mut self,
        pipeline: Arc<GraphBuildPipeline>,
        build_concurrency: usize,
    ) -> Self {
        assert!(
            build_concurrency > 0,
            "graph build concurrency must be non-zero"
        );
        self.graph_memory = Some(GraphMemoryRuntime {
            pipeline,
            build_concurrency,
        });
        self
    }

    pub async fn ingest(
        &self,
        principal: &Principal,
        request: IngestRequest,
    ) -> Result<IngestResponse, ServiceError> {
        request
            .validate()
            .map_err(|_| ServiceError::InvalidRequest)?;
        let scope_id = principal.scope_id();
        let entries = request
            .messages
            .iter()
            .filter(|message| message.candidate)
            .map(|message| IdempotencyEntry {
                scope_id: scope_id.clone(),
                conversation_id: request.conversation_id.clone(),
                message_id: message.id.clone(),
                content_hash: content_hash(message),
            })
            .collect::<Vec<_>>();
        let lock = self.ingest_lock(&scope_id, &request, &entries)?;
        let _guard = lock.lock().await;
        let proposed_run_id = format!("run-{}", Uuid::new_v4());
        let (pipeline_run_id, candidate_message_ids) = if entries.is_empty() {
            (proposed_run_id, HashSet::new())
        } else {
            match self
                .idempotency
                .reserve(&entries, &proposed_run_id)
                .await
                .map_err(map_idempotency_error)?
            {
                Reservation::Cached { results } => return cached_response(results),
                Reservation::Proceed {
                    pipeline_run_id,
                    candidate_message_ids,
                } => (pipeline_run_id, candidate_message_ids.into_iter().collect()),
            }
        };

        let prepared = build_prepared_input(principal, &request, &candidate_message_ids);
        let run = run_memory_pipeline(
            &prepared,
            &self.pipeline_config,
            self.extractor.as_ref(),
            self.verifier.as_ref(),
            None,
        )
        .await
        .map_err(|_| ServiceError::Pipeline)?;
        let requests = stored_memory_requests(&run.prepared, principal, &pipeline_run_id)?;
        let graph_requests = if self.graph_memory.is_some() {
            build_graph_add_requests(principal, &requests)?
        } else {
            Vec::new()
        };
        let memory_ids = self
            .manager
            .add_many(requests)
            .await
            .map_err(|_| ServiceError::Storage)?
            .into_iter()
            .map(|response| response.id)
            .collect::<Vec<_>>();
        if let Some(graph_memory) = &self.graph_memory {
            build_graph_memories(graph_memory, graph_requests).await?;
        }
        let response = IngestResponse {
            pipeline_run_id: pipeline_run_id.clone(),
            accepted_count: run.accepted_memories.len(),
            rejected_count: run.rejected.len(),
            quarantined_count: run.quarantined.len(),
            memory_ids,
            idempotency_hit: false,
            retriable: false,
        };
        if !entries.is_empty() {
            let pending_entries = entries
                .iter()
                .filter(|entry| candidate_message_ids.contains(&entry.message_id))
                .cloned()
                .collect::<Vec<_>>();
            let result = serde_json::to_value(&response).map_err(|_| ServiceError::Storage)?;
            self.idempotency
                .complete(&pending_entries, &pipeline_run_id, &result)
                .await
                .map_err(map_idempotency_error)?;
        }
        Ok(response)
    }

    pub async fn search(
        &self,
        principal: &Principal,
        request: SearchRequest,
    ) -> Result<SearchResponse, ServiceError> {
        request
            .validate()
            .map_err(|_| ServiceError::InvalidRequest)?;
        let top_k = request.top_k;
        let memory_types = request.memory_types.iter().cloned().collect::<HashSet<_>>();
        let event_time_from = request
            .event_time_from
            .as_deref()
            .and_then(parse_event_time);
        let event_time_to = request.event_time_to.as_deref().and_then(parse_event_time);
        let candidate_limit = bounded_candidate_limit(top_k);
        let candidates = self
            .manager
            .search(CoreSearchRequest {
                query: request.query,
                top_k: candidate_limit,
                filter: Some(json!({"scope_id": principal.scope_id()})),
                graph_memory_space_id: self.graph_memory.as_ref().map(|_| principal.scope_id()),
                graph_target_subject: None,
                graph_target_evidence_speaker: None,
            })
            .await
            .map_err(|_| ServiceError::Storage)?;
        let mut memories = candidates
            .into_iter()
            .map(|candidate| search_result(candidate.record, candidate.score))
            .filter(|memory| {
                matches_requested_predicates(
                    memory,
                    &memory_types,
                    event_time_from.as_ref(),
                    event_time_to.as_ref(),
                )
            })
            .collect::<Vec<_>>();
        memories.truncate(top_k);
        Ok(SearchResponse { memories })
    }

    fn ingest_lock(
        &self,
        scope_id: &str,
        request: &IngestRequest,
        entries: &[IdempotencyEntry],
    ) -> Result<Arc<AsyncMutex<()>>, ServiceError> {
        let mut message_ids = entries
            .iter()
            .map(|entry| entry.message_id.as_str())
            .collect::<Vec<_>>();
        message_ids.sort_unstable();
        let mut fields = vec![scope_id, request.conversation_id.as_str()];
        fields.extend(message_ids);
        let key = stable_tuple_hash(LOCK_KEY_VERSION, &fields);
        let mut locks = self
            .ingest_locks
            .lock()
            .map_err(|_| ServiceError::Storage)?;
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
            return Ok(lock);
        }
        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(key, Arc::downgrade(&lock));
        Ok(lock)
    }
}

fn stable_source_id(scope_id: &str, conversation_id: &str, message_id: &str) -> String {
    format!(
        "source-{}",
        stable_tuple_hash(SOURCE_ID_VERSION, &[scope_id, conversation_id, message_id])
    )
}

fn stable_session_id(scope_id: &str, conversation_id: &str) -> String {
    format!(
        "session-{}",
        stable_tuple_hash(SESSION_ID_VERSION, &[scope_id, conversation_id])
    )
}

fn stable_tuple_hash(version: &[u8], fields: &[&str]) -> String {
    let mut digest = Sha256::new();
    digest.update(version);
    for field in fields {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn content_hash(message: &IngestMessage) -> String {
    let encoded = serde_json::to_vec(message).expect("ingest messages are serializable");
    let mut digest = Sha256::new();
    digest.update(CONTENT_HASH_VERSION);
    digest.update((encoded.len() as u64).to_be_bytes());
    digest.update(encoded);
    format!("{:x}", digest.finalize())
}

fn map_idempotency_error(error: IdempotencyError) -> ServiceError {
    match error {
        IdempotencyError::Conflict => ServiceError::IdempotencyConflict,
        IdempotencyError::Storage => ServiceError::Storage,
    }
}

fn cached_response(results: Vec<Value>) -> Result<IngestResponse, ServiceError> {
    let mut responses = results
        .into_iter()
        .map(serde_json::from_value::<IngestResponse>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ServiceError::Storage)?;
    responses.sort_by(|left, right| left.pipeline_run_id.cmp(&right.pipeline_run_id));
    responses.dedup_by(|left, right| left.pipeline_run_id == right.pipeline_run_id);
    let pipeline_run_id = responses
        .first()
        .map(|response| response.pipeline_run_id.clone())
        .ok_or(ServiceError::Storage)?;
    let mut memory_ids = responses
        .iter()
        .flat_map(|response| response.memory_ids.iter().cloned())
        .collect::<Vec<_>>();
    memory_ids.sort();
    memory_ids.dedup();
    Ok(IngestResponse {
        pipeline_run_id,
        accepted_count: memory_ids.len(),
        rejected_count: responses
            .iter()
            .map(|response| response.rejected_count)
            .sum(),
        quarantined_count: responses
            .iter()
            .map(|response| response.quarantined_count)
            .sum(),
        memory_ids,
        idempotency_hit: true,
        retriable: false,
    })
}

fn stored_memory_requests(
    prepared: &Value,
    principal: &Principal,
    pipeline_run_id: &str,
) -> Result<Vec<AddMemoryRequest>, ServiceError> {
    let memories = prepared
        .get("memories")
        .and_then(Value::as_array)
        .ok_or(ServiceError::Pipeline)?;
    memories
        .iter()
        .map(|memory| {
            let id = memory
                .get("id")
                .and_then(Value::as_str)
                .ok_or(ServiceError::Pipeline)?;
            let text = memory
                .get("text")
                .and_then(Value::as_str)
                .ok_or(ServiceError::Pipeline)?;
            let mut metadata = memory
                .get("metadata")
                .and_then(Value::as_object)
                .cloned()
                .ok_or(ServiceError::Pipeline)?;
            metadata.insert("scope_id".to_string(), json!(principal.scope_id()));
            metadata.insert("source_agent_id".to_string(), json!(principal.agent_id));
            metadata.insert("pipeline_run_id".to_string(), json!(pipeline_run_id));
            Ok(AddMemoryRequest {
                id: Some(id.to_string()),
                text: text.to_string(),
                metadata: Value::Object(metadata),
            })
        })
        .collect()
}

fn build_graph_add_requests(
    principal: &Principal,
    requests: &[AddMemoryRequest],
) -> Result<Vec<GraphAddMemoryRequest>, ServiceError> {
    requests
        .iter()
        .map(|request| {
            let id = request.id.as_deref().ok_or(ServiceError::Pipeline)?;
            let mut metadata = request
                .metadata
                .as_object()
                .cloned()
                .ok_or(ServiceError::Pipeline)?;
            metadata.remove("pipeline_run_id");
            metadata.remove("source_agent_id");
            if !metadata.contains_key("graph_source_entity") {
                if let Some(speaker) = metadata
                    .get("speaker")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|speaker| is_named_source_actor(speaker))
                {
                    metadata.insert(
                        "graph_source_entity".to_string(),
                        json!({"name": speaker, "entity_type": "PERSON"}),
                    );
                }
            }
            let mut graph_request = GraphAddMemoryRequest {
                memory_space_id: principal.scope_id(),
                owner_id: principal.scope_id(),
                idempotency_key: String::new(),
                text: request.text.clone(),
                metadata: Value::Object(metadata),
                session_id: request
                    .metadata
                    .get("session_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|session_id| !session_id.is_empty())
                    .map(str::to_string),
                session_sequence: request.metadata.get("turn_index").and_then(Value::as_i64),
                source_kind: "atomic_memory".to_string(),
                source_ref: Some(id.to_string()),
                content_role: "memory".to_string(),
                created_by_agent_id: Some(principal.agent_id.clone()),
                observed_at_ms: request
                    .metadata
                    .get("observed_at_ms")
                    .and_then(Value::as_u64),
            };
            graph_request.idempotency_key = format!("{}:{}", id, graph_request.input_hash());
            Ok(graph_request)
        })
        .collect()
}

fn is_named_source_actor(speaker: &str) -> bool {
    let normalized = speaker.trim().to_ascii_lowercase();
    !normalized.is_empty()
        && !["user", "assistant", "system", "tool"]
            .iter()
            .any(|role| is_role_placeholder(&normalized, role))
}

fn is_role_placeholder(speaker: &str, role: &str) -> bool {
    let Some(suffix) = speaker.strip_prefix(role) else {
        return false;
    };
    suffix.is_empty()
        || suffix
            .chars()
            .all(|character| character.is_ascii_digit() || !character.is_alphanumeric())
}

async fn build_graph_memories(
    runtime: &GraphMemoryRuntime,
    requests: Vec<GraphAddMemoryRequest>,
) -> Result<(), ServiceError> {
    let mut requests = requests.into_iter();
    let mut tasks = JoinSet::new();
    for _ in 0..runtime.build_concurrency {
        let Some(request) = requests.next() else {
            break;
        };
        spawn_graph_build(&mut tasks, runtime.pipeline.clone(), request);
    }
    while let Some(result) = tasks.join_next().await {
        result
            .map_err(|_| ServiceError::Pipeline)?
            .map_err(|_| ServiceError::Pipeline)?;
        if let Some(request) = requests.next() {
            spawn_graph_build(&mut tasks, runtime.pipeline.clone(), request);
        }
    }
    Ok(())
}

fn spawn_graph_build(
    tasks: &mut JoinSet<memory_core::MemoryResult<Option<memory_core::GraphBuildResult>>>,
    pipeline: Arc<GraphBuildPipeline>,
    request: GraphAddMemoryRequest,
) {
    tasks.spawn(async move { pipeline.resume_memory(request).await });
}

fn search_result(record: MemoryRecord, score: f32) -> SearchResult {
    SearchResult {
        id: record.id,
        text: record.text,
        memory_type: metadata_text(&record.metadata, "memory_type"),
        modality: metadata_text(&record.metadata, "modality"),
        event_time: record
            .metadata
            .get("event_time")
            .cloned()
            .unwrap_or(Value::Null),
        observed_at: metadata_text(&record.metadata, "observed_at"),
        evidence_refs: record
            .metadata
            .get("evidence_refs")
            .cloned()
            .unwrap_or_else(|| json!([])),
        source_agent_id: metadata_text(&record.metadata, "source_agent_id"),
        graph_facts: record
            .metadata
            .get("graph_facts")
            .cloned()
            .unwrap_or_else(|| json!([])),
        graph_facts_truncated: record
            .metadata
            .get("graph_facts_truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        score,
    }
}

fn metadata_text(metadata: &Value, key: &str) -> String {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn bounded_candidate_limit(top_k: usize) -> usize {
    top_k
        .saturating_mul(5)
        .max(top_k)
        .min(MAX_SEARCH_CANDIDATES)
}

fn matches_requested_predicates(
    memory: &SearchResult,
    memory_types: &HashSet<String>,
    event_time_from: Option<&DateTime<Utc>>,
    event_time_to: Option<&DateTime<Utc>>,
) -> bool {
    if !memory_types.is_empty() && !memory_types.contains(&memory.memory_type) {
        return false;
    }
    if event_time_from.is_none() && event_time_to.is_none() {
        return true;
    }
    let Some(event_time) = event_time_from_value(&memory.event_time) else {
        return false;
    };
    event_time_from.is_none_or(|from| event_time >= *from)
        && event_time_to.is_none_or(|to| event_time <= *to)
}

fn event_time_from_value(value: &Value) -> Option<DateTime<Utc>> {
    value
        .as_str()
        .and_then(parse_event_time)
        .or_else(|| {
            value
                .get("normalized")
                .and_then(Value::as_str)
                .and_then(parse_event_time)
        })
        .or_else(|| {
            value
                .get("raw")
                .and_then(Value::as_str)
                .and_then(parse_event_time)
        })
}

fn parse_event_time(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .ok()?
                .and_hms_opt(0, 0, 0)
                .map(|value| value.and_utc())
        })
}

fn build_prepared_input(
    principal: &Principal,
    request: &IngestRequest,
    candidate_message_ids: &HashSet<String>,
) -> Value {
    let scope_id = principal.scope_id();
    let session_id = stable_session_id(&scope_id, &request.conversation_id);
    let memories = request
        .messages
        .iter()
        .enumerate()
        .map(|(turn_index, message)| {
            json!({
                "id": stable_source_id(&scope_id, &request.conversation_id, &message.id),
                "text": message.text,
                "metadata": {
                    "scope_id": scope_id,
                    "session_id": session_id,
                    "role": message.role,
                    "speaker": message.speaker.as_deref().unwrap_or(&message.role),
                    "timestamp": message.timestamp.as_deref().unwrap_or(""),
                    "turn_index": turn_index,
                    "source_agent_id": principal.agent_id,
                    "memory_candidate": message.candidate
                        && candidate_message_ids.contains(&message.id),
                }
            })
        })
        .collect::<Vec<_>>();
    json!({
        "schema_version": "benchmark-prepared-v1",
        "dataset": {"name": "memory-mcp", "split": "online"},
        "memories": memories,
        "queries": [],
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    use memory_core::{
        AddMemoryRequest, HashEmbedding, MemoryManager, MemoryRecord, SqliteMemoryStore,
    };
    use memory_pipeline::extraction::StaticMemoryExtractor;
    use memory_pipeline::grounding::StaticGroundingVerifier;
    use serde_json::json;

    use super::{
        bounded_candidate_limit, build_graph_add_requests, build_prepared_input, search_result,
        stable_source_id, MemoryService,
    };
    use crate::{IdempotencyRepository, IngestMessage, IngestRequest, Principal};

    fn principal(user: &str, agent: &str) -> Principal {
        Principal {
            tenant_id: "tenant-a".to_string(),
            user_id: user.to_string(),
            agent_id: agent.to_string(),
            permissions: Vec::new(),
        }
    }

    fn request() -> IngestRequest {
        IngestRequest {
            conversation_id: "conversation-raw".to_string(),
            messages: vec![
                IngestMessage {
                    id: "history-raw".to_string(),
                    role: "assistant".to_string(),
                    speaker: Some("Helper".to_string()),
                    text: "Earlier context.".to_string(),
                    timestamp: Some("2026-07-22T09:59:00Z".to_string()),
                    candidate: false,
                },
                IngestMessage {
                    id: "candidate-raw".to_string(),
                    role: "user".to_string(),
                    speaker: Some("Alice".to_string()),
                    text: "I prefer a window seat.".to_string(),
                    timestamp: Some("2026-07-22T10:00:00Z".to_string()),
                    candidate: true,
                },
            ],
        }
    }

    async fn lock_test_service() -> (
        tempfile::TempDir,
        MemoryService<StaticMemoryExtractor, StaticGroundingVerifier>,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("memory.sqlite");
        let manager = Arc::new(MemoryManager::new(
            Arc::new(SqliteMemoryStore::new(&database_path)),
            Arc::new(HashEmbedding::new(32)),
        ));
        let idempotency = IdempotencyRepository::open(&database_path).await.unwrap();
        let service = MemoryService::new(
            manager,
            idempotency,
            Arc::new(StaticMemoryExtractor::new(HashMap::new())),
            Arc::new(StaticGroundingVerifier::new(HashMap::new())),
        );
        (temp, service)
    }

    #[tokio::test]
    async fn ingest_lock_registry_prunes_dead_keys_while_preserving_live_locks() {
        let (_temp, service) = lock_test_service().await;
        let scope_id = principal("alice", "agent-a").scope_id();
        let active_request = request();
        let active = service
            .ingest_lock(&scope_id, &active_request, &[])
            .unwrap();

        for index in 0..8 {
            let mut stale_request = request();
            stale_request.conversation_id = format!("stale-{index}");
            let stale = service.ingest_lock(&scope_id, &stale_request, &[]).unwrap();
            drop(stale);
        }

        let mut current_request = request();
        current_request.conversation_id = "current".to_string();
        let current = service
            .ingest_lock(&scope_id, &current_request, &[])
            .unwrap();
        let active_again = service
            .ingest_lock(&scope_id, &active_request, &[])
            .unwrap();

        assert!(Arc::ptr_eq(&active, &active_again));
        let registry = service.ingest_locks.lock().unwrap();
        assert_eq!(registry.len(), 2);
        assert!(registry.values().all(|entry| entry.upgrade().is_some()));
        drop(registry);

        let current_again = service
            .ingest_lock(&scope_id, &current_request, &[])
            .unwrap();
        assert!(Arc::ptr_eq(&current, &current_again));
    }

    #[test]
    fn source_ids_are_stable_opaque_and_bound_to_authenticated_scope() {
        let alice_a = principal("alice", "agent-a");
        let alice_b = principal("alice", "agent-b");
        let bob = principal("bob", "agent-a");
        let first = stable_source_id(&alice_a.scope_id(), "conversation-raw", "message-raw");

        assert_eq!(
            first,
            stable_source_id(&alice_b.scope_id(), "conversation-raw", "message-raw")
        );
        assert_ne!(
            first,
            stable_source_id(&bob.scope_id(), "conversation-raw", "message-raw")
        );
        assert!(first.starts_with("source-"));
        assert!(!first.contains("conversation-raw"));
        assert!(!first.contains("message-raw"));
    }

    #[test]
    fn prepared_input_carries_context_and_trusted_metadata() {
        let principal = principal("alice", "agent-a");
        let prepared = build_prepared_input(
            &principal,
            &request(),
            &HashSet::from(["candidate-raw".to_string()]),
        );

        assert_eq!(prepared["schema_version"], json!("benchmark-prepared-v1"));
        assert_eq!(prepared["memories"].as_array().unwrap().len(), 2);
        let history_metadata = &prepared["memories"][0]["metadata"];
        assert_eq!(history_metadata["role"], json!("assistant"));
        assert_eq!(history_metadata["speaker"], json!("Helper"));
        assert_eq!(history_metadata["timestamp"], json!("2026-07-22T09:59:00Z"));
        assert_eq!(history_metadata["turn_index"], json!(0));
        let candidate_metadata = &prepared["memories"][1]["metadata"];
        assert_eq!(candidate_metadata["role"], json!("user"));
        assert_eq!(candidate_metadata["speaker"], json!("Alice"));
        assert_eq!(
            candidate_metadata["timestamp"],
            json!("2026-07-22T10:00:00Z")
        );
        assert_eq!(candidate_metadata["turn_index"], json!(1));
        assert_eq!(
            prepared["memories"][0]["metadata"]["memory_candidate"],
            json!(false)
        );
        assert_eq!(
            prepared["memories"][1]["metadata"]["memory_candidate"],
            json!(true)
        );
        for memory in prepared["memories"].as_array().unwrap() {
            let metadata = &memory["metadata"];
            assert_eq!(metadata["scope_id"], json!(principal.scope_id()));
            assert_eq!(metadata["source_agent_id"], json!("agent-a"));
            assert!(metadata["session_id"]
                .as_str()
                .unwrap()
                .starts_with("session-"));
            assert!(memory["id"].as_str().unwrap().starts_with("source-"));
            assert!(!memory.to_string().contains("conversation-raw"));
            assert!(!memory.to_string().contains("candidate-raw"));
            assert!(!memory.to_string().contains("history-raw"));
        }
    }

    #[test]
    fn post_filter_candidate_pool_is_bounded() {
        assert_eq!(bounded_candidate_limit(1), 5);
        assert_eq!(bounded_candidate_limit(10), 50);
        assert_eq!(bounded_candidate_limit(100), 500);
        assert_eq!(bounded_candidate_limit(usize::MAX), 500);
    }

    #[test]
    fn graph_requests_preserve_atomic_memory_source_context() {
        let principal = principal("alice", "agent-a");
        let requests = vec![AddMemoryRequest {
            id: Some("memory-1".to_string()),
            text: "Alice prefers a window seat.".to_string(),
            metadata: json!({
                "speaker": "Alice",
                "session_id": "session-1",
                "turn_index": 7,
                "observed_at_ms": 1_785_000_000_000_u64,
                "source_agent_id": "agent-a",
                "pipeline_run_id": "transient-run-1",
                "scope_id": principal.scope_id()
            }),
        }];

        let graph = build_graph_add_requests(&principal, &requests).unwrap();

        assert_eq!(graph.len(), 1);
        assert_eq!(graph[0].memory_space_id, principal.scope_id());
        assert_eq!(graph[0].owner_id, principal.scope_id());
        assert!(graph[0].idempotency_key.starts_with("memory-1:"));
        assert_eq!(graph[0].source_ref.as_deref(), Some("memory-1"));
        assert_eq!(graph[0].source_kind, "atomic_memory");
        assert_eq!(graph[0].content_role, "memory");
        assert_eq!(graph[0].created_by_agent_id.as_deref(), Some("agent-a"));
        assert_eq!(graph[0].session_id.as_deref(), Some("session-1"));
        assert_eq!(graph[0].session_sequence, Some(7));
        assert_eq!(graph[0].observed_at_ms, Some(1_785_000_000_000));
        assert_eq!(
            graph[0].metadata["graph_source_entity"],
            json!({"name": "Alice", "entity_type": "PERSON"})
        );
        assert!(graph[0].metadata.get("source_agent_id").is_none());
        assert!(graph[0].metadata.get("pipeline_run_id").is_none());
    }

    #[test]
    fn generic_role_speakers_are_not_graph_source_entities() {
        for speaker in ["user", "User1", "assistant-2", "system_3", "tool 4"] {
            assert!(!super::is_named_source_actor(speaker), "speaker={speaker}");
        }
        for speaker in ["Alice", "Assistant Professor Lee", "Systema"] {
            assert!(super::is_named_source_actor(speaker), "speaker={speaker}");
        }
    }

    #[test]
    fn search_results_expose_graph_facts_without_changing_memory_text() {
        let graph_facts = json!([{
            "fact_id": "fact-1",
            "fact_text": "Alice prefers a window seat.",
            "predicate": "LIKES"
        }]);
        let result = search_result(
            MemoryRecord {
                id: "memory-1".to_string(),
                text: "Window seats are my preference.".to_string(),
                metadata: json!({
                    "graph_facts": graph_facts,
                    "graph_facts_truncated": true
                }),
                embedding: vec![],
                created_at_ms: 1,
                updated_at_ms: 1,
            },
            0.9,
        );

        assert_eq!(result.text, "Window seats are my preference.");
        assert_eq!(result.graph_facts, graph_facts);
        assert!(result.graph_facts_truncated);
    }
}
