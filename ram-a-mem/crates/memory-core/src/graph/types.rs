use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySpaceStatus {
    Active,
    Deleting,
    Deleted,
    Purged,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactStatus {
    Active,
    Superseded,
    Retracted,
    Unsupported,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactLinkType {
    Supersedes,
    Contradicts,
    DerivedFrom,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemorySpace {
    pub id: String,
    pub owner_id: String,
    pub status: MemorySpaceStatus,
    pub next_ingestion_sequence: i64,
    pub created_at_ms: u64,
    pub deleted_at_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphMemoryRecord {
    pub id: String,
    pub memory_space_id: String,
    pub session_id: Option<String>,
    pub ingestion_sequence: i64,
    pub session_sequence: Option<i64>,
    pub text: String,
    pub metadata: serde_json::Value,
    pub source_kind: String,
    pub source_ref: Option<String>,
    pub content_role: String,
    pub created_by_agent_id: Option<String>,
    pub observed_at_ms: Option<u64>,
    pub embedding: Option<Vec<f32>>,
    pub embedding_model: Option<String>,
    pub embedding_version: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub deleted_at_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    pub memory_space_id: String,
    pub canonical_name: String,
    pub normalized_name: String,
    pub entity_type: String,
    pub name_embedding: Option<Vec<f32>>,
    pub embedding_model: Option<String>,
    pub embedding_version: Option<String>,
    pub status: String,
    pub type_registry_version: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub deleted_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EntityAlias {
    pub id: String,
    pub memory_space_id: String,
    pub entity_id: String,
    pub display_alias: String,
    pub normalized_alias: String,
    pub created_at_ms: u64,
    pub deleted_at_ms: Option<u64>,
}

/// A caller-declared entity that authored or otherwise originated a source record.
///
/// This is provenance, not an extracted fact. Callers may provide it in record
/// metadata as `graph_source_entity` with `name` and `entity_type` fields.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GraphSourceEntity {
    pub name: String,
    pub entity_type: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EntityAliasEvidence {
    pub id: String,
    pub memory_space_id: String,
    pub entity_alias_id: String,
    pub source_kind: String,
    pub memory_record_id: Option<String>,
    pub registry_version: Option<String>,
    pub extraction_run_id: Option<String>,
    pub created_at_ms: u64,
    pub deleted_at_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fact {
    pub id: String,
    pub memory_space_id: String,
    pub subject_entity_id: String,
    pub predicate: String,
    pub object_entity_id: String,
    pub fact_text: String,
    pub dedup_key: Option<String>,
    pub embedding: Option<Vec<f32>>,
    pub embedding_model: Option<String>,
    pub embedding_version: Option<String>,
    pub status: FactStatus,
    pub valid_from_ms: Option<u64>,
    pub valid_to_ms: Option<u64>,
    pub recorded_at_ms: u64,
    pub retired_at_ms: Option<u64>,
    pub type_registry_version: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FactEvidenceGroup {
    pub id: String,
    pub memory_space_id: String,
    pub fact_id: String,
    pub evidence_kind: String,
    pub extraction_run_id: Option<String>,
    pub created_at_ms: u64,
    pub deleted_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FactEvidence {
    pub id: String,
    pub memory_space_id: String,
    pub evidence_group_id: String,
    pub memory_record_id: String,
    pub evidence_text: Option<String>,
    pub start_byte: Option<usize>,
    pub end_byte: Option<usize>,
    pub created_at_ms: u64,
    pub deleted_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FactLink {
    pub id: String,
    pub memory_space_id: String,
    pub from_fact_id: String,
    pub link_type: FactLinkType,
    pub to_fact_id: String,
    pub created_at_ms: u64,
    pub deleted_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FactLinkEvidenceGroup {
    pub id: String,
    pub memory_space_id: String,
    pub fact_link_id: String,
    pub extraction_run_id: Option<String>,
    pub created_at_ms: u64,
    pub deleted_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FactLinkEvidence {
    pub id: String,
    pub memory_space_id: String,
    pub evidence_group_id: String,
    pub memory_record_id: String,
    pub evidence_text: Option<String>,
    pub start_byte: Option<usize>,
    pub end_byte: Option<usize>,
    pub created_at_ms: u64,
    pub deleted_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FactStatusHistory {
    pub id: String,
    pub memory_space_id: String,
    pub fact_id: String,
    pub old_status: Option<FactStatus>,
    pub new_status: FactStatus,
    pub reason_code: String,
    pub trigger_record_id: Option<String>,
    pub trigger_fact_link_id: Option<String>,
    pub created_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResolutionDecision {
    pub id: String,
    pub memory_space_id: String,
    pub ingestion_run_id: String,
    pub decision_kind: String,
    pub input_key: String,
    pub candidate_ids: Vec<String>,
    pub selected_id: Option<String>,
    pub action: String,
    pub method: String,
    pub model: Option<String>,
    pub resolver_version: String,
    pub created_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IngestionRun {
    pub id: String,
    pub memory_space_id: String,
    pub memory_record_id: String,
    pub idempotency_key: String,
    pub input_hash: String,
    pub status: String,
    pub stage: String,
    pub attempt_count: i64,
    pub pipeline_version: String,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at_ms: u64,
    pub started_at_ms: Option<u64>,
    pub updated_at_ms: u64,
    pub completed_at_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExtractionRun {
    pub id: String,
    pub memory_space_id: String,
    pub ingestion_run_id: String,
    pub attempt_number: i64,
    pub status: String,
    pub extractor_name: String,
    pub model: String,
    pub prompt_version: String,
    pub schema_version: String,
    pub type_registry_version: String,
    pub context_record_ids: Vec<String>,
    pub structured_output: Option<serde_json::Value>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub latency_ms: Option<i64>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at_ms: u64,
    pub completed_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FactEdgeProjection {
    pub memory_space_id: String,
    pub fact_id: String,
    pub source_entity_id: String,
    pub target_entity_id: String,
    pub predicate: String,
    pub status: FactStatus,
    pub valid_from_ms: Option<u64>,
    pub valid_to_ms: Option<u64>,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FactContextUnit {
    pub fact_id: String,
    pub fact_text: String,
    pub subject_entity: Entity,
    pub object_entity: Entity,
    pub predicate: String,
    pub evidence_records: Vec<GraphMemoryRecord>,
    pub fact_links: Vec<FactLink>,
    pub path: Vec<String>,
    pub score: f32,
    pub status: FactStatus,
    pub valid_from_ms: Option<u64>,
    pub valid_to_ms: Option<u64>,
    pub recorded_at_ms: u64,
}

/// A source record matched directly inside the graph store.
///
/// This is deliberately separate from `FactContextUnit`: it preserves an
/// unextracted source as graph evidence without presenting it as a fact.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvidenceRecordContextUnit {
    pub record: GraphMemoryRecord,
    pub path: Vec<String>,
    pub score: f32,
    pub match_kind: EvidenceRecordMatchKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceRecordMatchKind {
    Lexical,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextBundle {
    pub query: String,
    pub memory_space_id: String,
    pub reference_time_ms: u64,
    pub fact_context_units: Vec<FactContextUnit>,
    pub evidence_record_context_units: Vec<EvidenceRecordContextUnit>,
    pub records: Vec<GraphMemoryRecord>,
    pub entities: Vec<Entity>,
    pub facts: Vec<Fact>,
    pub fact_links: Vec<FactLink>,
    pub paths: Vec<Vec<String>>,
    pub truncation: Option<String>,
    pub degraded_reason: Option<String>,
}
