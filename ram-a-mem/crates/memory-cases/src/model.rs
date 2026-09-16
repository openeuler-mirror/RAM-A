use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize)]
pub struct Dataset {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Document {
    pub id: String,
    pub dataset_id: String,
    pub name: String,
    pub file_path: String,
    pub mime_type: Option<String>,
    pub size_bytes: u64,
    pub status: String,
    pub chunk_count: usize,
    pub error: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct IngestionTask {
    pub id: String,
    pub dataset_id: String,
    pub document_id: String,
    pub status: String,
    pub error: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub started_at_ms: Option<u64>,
    pub completed_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexRebuildState {
    Queued,
    Running,
    Completed,
    Failed,
}

impl IndexRebuildState {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
}

/// Pipeline stage of an active index rebuild.
///
/// Terminal outcomes are expressed by [`IndexRebuildState`] alone; a rebuild
/// that reaches `completed` or `failed` keeps the last stage it progressed
/// through, so there is no way for the phase and the state to disagree.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexRebuildPhase {
    Queued,
    ScanningBusinessDatabase,
    Embedding,
    Validating,
    Swapping,
}

#[derive(Clone, Debug, Serialize)]
pub struct IndexRebuildStatus {
    pub operation_id: String,
    pub state: IndexRebuildState,
    pub phase: IndexRebuildPhase,
    pub document_count: usize,
    pub chunk_count: usize,
    pub record_count: usize,
    pub processed_record_count: usize,
    pub started_at_ms: u64,
    pub completed_at_ms: Option<u64>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Chunk {
    pub id: String,
    pub dataset_id: String,
    pub document_id: String,
    pub chunk_index: usize,
    pub content: String,
    pub chunk_type: String,
    pub token_count: usize,
    pub parse_topology: String,
    pub source_node_indices: Vec<usize>,
    pub available: bool,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug)]
pub struct StoredDocument {
    pub id: String,
    pub dataset_id: String,
    pub name: String,
    pub file_path: String,
    pub mime_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateDatasetRequest {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ListDatasetsResponse {
    pub datasets: Vec<Dataset>,
}

#[derive(Debug)]
pub struct CreateDocumentFileRequest {
    pub id: Option<String>,
    pub task_id: Option<String>,
    pub name: String,
    pub file_name: String,
    pub mime_type: Option<String>,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Serialize)]
pub struct CreateDocumentResponse {
    pub document_id: String,
    pub task_id: String,
}

#[derive(Debug)]
pub struct UpdateDocumentFileRequest {
    pub task_id: Option<String>,
    pub name: Option<String>,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
    pub bytes: Option<Vec<u8>>,
}

#[derive(Debug, Serialize)]
pub struct UpdateDocumentResponse {
    pub document_id: String,
    pub task_id: String,
}

#[derive(Debug, Serialize)]
pub struct DeleteDocumentResponse {
    pub document_id: String,
    pub deleted: bool,
}

#[derive(Debug, Serialize)]
pub struct ListDocumentsResponse {
    pub documents: Vec<Document>,
}

#[derive(Debug, Serialize)]
pub struct ListChunksResponse {
    pub chunks: Vec<Chunk>,
    pub total: usize,
}

#[derive(Debug, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default = "default_top_k")]
    pub top_k: usize,
}

#[derive(Debug, Serialize)]
pub struct SearchChunk {
    pub chunk_id: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub context_chunk_ids: Vec<String>,
    pub dataset_id: String,
    pub document_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    pub content: String,
    pub score: f32,
}

#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub chunks: Vec<SearchChunk>,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionRequest {
    pub dataset_id: String,
    pub question: String,
    #[serde(default = "default_top_k")]
    pub top_k: usize,
}

#[derive(Debug, Serialize)]
pub struct ChatCompletionResponse {
    pub answer: String,
    pub references: Vec<SearchChunk>,
}

fn default_top_k() -> usize {
    5
}
