use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use memory_core::{
    AddMemoryRequest, LongTermMemory, MemoryManager, ScoredMemory, SearchMemoryRequest,
};
use tokio::fs;
use uuid::Uuid;

use crate::chunker::{chunk_parse_result, ChunkerConfig};
use crate::llm::DocumentSummaryClient;
use crate::model::{
    ChatCompletionResponse, Chunk, CreateDatasetRequest, CreateDocumentFileRequest,
    CreateDocumentResponse, Dataset, DeleteDocumentResponse, IngestionTask, ListChunksResponse,
    ListDatasetsResponse, ListDocumentsResponse, SearchChunk, SearchRequest, SearchResponse,
    StoredDocument, UpdateDocumentFileRequest, UpdateDocumentResponse,
};
use crate::parser::ParserEngine;
use crate::repo::{current_time_ms, RagRepository};
use crate::token_counter::TokenCounter;

const CHUNKING_STRATEGY: &str = "parser_node_chunking_v2";
const CHUNK_RECORD_KIND: &str = "chunk";
const DOCUMENT_SUMMARY_RECORD_KIND: &str = "document_summary";
const MEMORY_INDEX_NAMESPACE: &str = "memory-cases";
const DOCUMENT_SUMMARY_MAX_CHARS: usize = 6000;
const DOCUMENT_SUMMARY_BODY_MAX_CHARS: usize = 1200;
const DOCUMENT_SUMMARY_CANDIDATE_MAX_CHARS: usize = 180;
const DOCUMENT_SUMMARY_MAX_LINES: usize = 8;
const LLM_DOCUMENT_SUMMARY_SOURCE_MAX_CHARS: usize = 12000;
const DOCUMENT_RECALL_MIN_CANDIDATES: usize = 20;
const DOCUMENT_RECALL_MULTIPLIER: usize = 4;
const MIN_RELEVANCE_SCORE: f32 = 0.45;
const MIN_ASCII_IDENTIFIER_QUERY_TERMS: usize = 2;
const MIN_ASCII_IDENTIFIER_OVERLAP_TERMS: usize = 2;
const SEARCH_TEXT_REPLACEMENTS: &[(&str, &str)] = &[("蓝芽", "蓝牙")];
const DOCUMENT_SUMMARY_SIGNAL_TERMS: &[&str] = &[
    "问题", "现象", "原因", "根因", "方案", "步骤", "处理", "解决", "修复", "排查", "注意", "结论",
    "建议", "方法", "异常", "失败", "错误", "检查", "打开", "点击", "关闭", "启用", "禁用", "安装",
];

#[derive(Clone)]
pub struct RagConfig {
    pub file_root: PathBuf,
    pub chunk_size: usize,
    pub token_counter: Arc<dyn TokenCounter>,
    pub summary_llm: Option<DocumentSummaryClient>,
}

pub struct RagService {
    repo: Arc<RagRepository>,
    memory: Arc<MemoryManager>,
    config: RagConfig,
}

impl RagService {
    pub fn new(repo: Arc<RagRepository>, memory: Arc<MemoryManager>, config: RagConfig) -> Self {
        Self {
            repo,
            memory,
            config,
        }
    }

    pub fn create_dataset(&self, request: CreateDatasetRequest) -> Result<Dataset> {
        self.repo.create_dataset(
            request.id.as_deref(),
            &request.name,
            request.description.as_deref(),
        )
    }

    pub fn list_datasets(&self) -> Result<ListDatasetsResponse> {
        Ok(ListDatasetsResponse {
            datasets: self.repo.list_datasets()?,
        })
    }

    pub async fn create_document(
        &self,
        dataset_id: &str,
        request: CreateDocumentFileRequest,
    ) -> Result<CreateDocumentResponse> {
        self.ensure_dataset_exists(dataset_id)?;

        let document_id = request
            .id
            .unwrap_or_else(|| Uuid::new_v4().to_string())
            .trim()
            .to_string();
        let task_id = request
            .task_id
            .unwrap_or_else(|| Uuid::new_v4().to_string())
            .trim()
            .to_string();
        anyhow::ensure!(!document_id.is_empty(), "document id must not be empty");
        anyhow::ensure!(!task_id.is_empty(), "task id must not be empty");
        anyhow::ensure!(!request.bytes.is_empty(), "document file must not be empty");

        let file_name = safe_file_name(&request.file_name)?;
        let file_path = self
            .config
            .file_root
            .join(dataset_id)
            .join(&document_id)
            .join(&file_name);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(&file_path, &request.bytes).await?;

        let name = if request.name.trim().is_empty() {
            file_name
        } else {
            request.name.trim().to_string()
        };
        let (_document, task) = self.repo.create_document_with_task(
            dataset_id,
            Some(&document_id),
            Some(&task_id),
            &name,
            file_path.to_str().with_context(|| {
                format!("file path is not valid UTF-8: {}", file_path.display())
            })?,
            request.mime_type.as_deref(),
            request.bytes.len() as u64,
        )?;
        Ok(CreateDocumentResponse {
            document_id: task.document_id,
            task_id: task.id,
        })
    }

    pub fn list_documents(&self, dataset_id: &str) -> Result<ListDocumentsResponse> {
        self.ensure_dataset_exists(dataset_id)?;
        Ok(ListDocumentsResponse {
            documents: self.repo.list_documents(dataset_id)?,
        })
    }

    pub async fn update_document(
        &self,
        dataset_id: &str,
        document_id: &str,
        request: UpdateDocumentFileRequest,
    ) -> Result<UpdateDocumentResponse> {
        self.ensure_dataset_exists(dataset_id)?;
        let existing = self
            .repo
            .get_stored_document(document_id)?
            .filter(|document| document.dataset_id == dataset_id)
            .with_context(|| format!("document not found: {document_id}"))?;
        let name = request
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| existing.name.clone());

        let task_id = request
            .task_id
            .unwrap_or_else(|| Uuid::new_v4().to_string())
            .trim()
            .to_string();
        anyhow::ensure!(!task_id.is_empty(), "task id must not be empty");

        let (file_path, mime_type, size_bytes) = if let Some(bytes) = request.bytes {
            anyhow::ensure!(!bytes.is_empty(), "document file must not be empty");
            let file_name = request
                .file_name
                .as_deref()
                .context("file name is required when updating document file")
                .and_then(safe_file_name)?;
            let file_path = self
                .config
                .file_root
                .join(dataset_id)
                .join(document_id)
                .join(&file_name);
            if let Some(parent) = file_path.parent() {
                fs::create_dir_all(parent).await?;
            }
            fs::write(&file_path, &bytes).await?;
            (
                file_path
                    .to_str()
                    .with_context(|| {
                        format!("file path is not valid UTF-8: {}", file_path.display())
                    })?
                    .to_string(),
                request.mime_type.or_else(|| existing.mime_type.clone()),
                bytes.len() as u64,
            )
        } else {
            anyhow::ensure!(
                request
                    .name
                    .as_deref()
                    .map(str::trim)
                    .is_some_and(|value| !value.is_empty()),
                "document update requires name or file"
            );
            (
                existing.file_path.clone(),
                existing.mime_type.clone(),
                std::fs::metadata(&existing.file_path)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0),
            )
        };

        let (_document, task) = self.repo.update_document_with_task(
            dataset_id,
            document_id,
            Some(&task_id),
            &name,
            &file_path,
            mime_type.as_deref(),
            size_bytes,
        )?;
        self.delete_document_search_records(dataset_id, document_id)
            .await?;
        remove_replaced_file(&existing.file_path, &file_path).await?;

        Ok(UpdateDocumentResponse {
            document_id: task.document_id,
            task_id: task.id,
        })
    }

    pub async fn delete_document(
        &self,
        dataset_id: &str,
        document_id: &str,
    ) -> Result<DeleteDocumentResponse> {
        self.ensure_dataset_exists(dataset_id)?;
        let existing = self
            .repo
            .get_stored_document(document_id)?
            .filter(|document| document.dataset_id == dataset_id)
            .with_context(|| format!("document not found: {document_id}"))?;

        self.delete_document_search_records(dataset_id, document_id)
            .await?;
        self.repo
            .delete_document(dataset_id, document_id)?
            .with_context(|| format!("document not found: {document_id}"))?;
        remove_document_file_dir(&existing.file_path).await?;

        Ok(DeleteDocumentResponse {
            document_id: document_id.to_string(),
            deleted: true,
        })
    }

    pub fn get_task(&self, task_id: &str) -> Result<Option<IngestionTask>> {
        self.repo.get_task(task_id)
    }

    pub fn list_chunks(&self, dataset_id: &str, document_id: &str) -> Result<ListChunksResponse> {
        self.ensure_dataset_exists(dataset_id)?;
        let chunks = self.repo.list_chunks(dataset_id, document_id)?;
        Ok(ListChunksResponse {
            total: chunks.len(),
            chunks,
        })
    }

    pub async fn search_dataset(
        &self,
        dataset_id: &str,
        request: SearchRequest,
    ) -> Result<SearchResponse> {
        self.ensure_dataset_exists(dataset_id)?;
        let query = request.query.trim();
        anyhow::ensure!(!query.is_empty(), "query must not be empty");
        if request.top_k == 0 {
            return Ok(SearchResponse { chunks: Vec::new() });
        }

        let results = self
            .memory
            .search(SearchMemoryRequest {
                query: build_search_query_text(query),
                top_k: document_recall_candidate_top_k(request.top_k),
                filter: Some(serde_json::json!({ "scope_id": dataset_id })),
            })
            .await?;
        let results = filter_unrelated_results(query, results);
        let results = filter_low_relevance_results(results);

        Ok(SearchResponse {
            chunks: self.expand_retrieved_documents(dataset_id, results, request.top_k)?,
        })
    }

    pub async fn chat_completion(
        &self,
        request: crate::model::ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse> {
        let search = self
            .search_dataset(
                &request.dataset_id,
                SearchRequest {
                    query: request.question.clone(),
                    top_k: request.top_k,
                },
            )
            .await?;

        let answer = if search.chunks.is_empty() {
            "No relevant content was found.".to_string()
        } else {
            let preview = search
                .chunks
                .iter()
                .map(|chunk| {
                    let source = chunk
                        .source_name
                        .as_deref()
                        .or(chunk.source_path.as_deref())
                        .unwrap_or(&chunk.document_id);
                    format!(
                        "Source: {}\n{}",
                        source,
                        truncate_chars(&chunk.content, 180)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            format!(
                "Found {} relevant chunks. Main retrieved content for answer augmentation:\n{}",
                search.chunks.len(),
                preview
            )
        };

        Ok(ChatCompletionResponse {
            answer,
            references: search.chunks,
        })
    }

    pub async fn run_next_ingestion_task(&self) -> Result<bool> {
        let Some(task) = self.repo.lease_next_pending_task()? else {
            return Ok(false);
        };

        match self.ingest_task(&task).await {
            Ok(chunk_count) => {
                self.repo
                    .complete_task(&task.id, &task.document_id, chunk_count)?;
            }
            Err(error) => {
                let message = error.to_string();
                self.repo
                    .fail_task(&task.id, &task.document_id, &message)
                    .with_context(|| format!("failed to mark task {} failed", task.id))?;
                return Err(error);
            }
        }

        Ok(true)
    }

    async fn ingest_task(&self, task: &IngestionTask) -> Result<usize> {
        let document = self
            .repo
            .get_stored_document(&task.document_id)?
            .with_context(|| format!("document {} not found", task.document_id))?;
        let chunk_document = document.clone();
        let chunk_config = self.config.clone();
        let chunks = tokio::task::spawn_blocking(move || {
            build_chunks_from_file(&chunk_document, &chunk_config)
        })
        .await
        .context("chunking worker failed to join")?
        .with_context(|| format!("failed to chunk document file {}", document.file_path))?;
        self.repo
            .replace_chunks(&document.dataset_id, &document.id, &chunks)?;
        let requests = self
            .build_memory_requests_for_document(&document, &chunks)
            .await;
        self.memory.add_many(requests).await?;
        Ok(chunks.len())
    }

    async fn delete_document_search_records(
        &self,
        dataset_id: &str,
        document_id: &str,
    ) -> Result<usize> {
        self.memory
            .delete_by_filters(document_memory_delete_filters(dataset_id, document_id))
            .await
            .map_err(Into::into)
    }

    async fn build_memory_requests_for_document(
        &self,
        document: &StoredDocument,
        chunks: &[Chunk],
    ) -> Vec<AddMemoryRequest> {
        let mut requests = chunks
            .iter()
            .map(|chunk| build_chunk_memory_request(document, chunk))
            .collect::<Vec<_>>();

        if !chunks.is_empty() {
            let (document_search_text, document_summary_source) = self
                .build_document_search_text_for_ingest(document, chunks)
                .await;
            requests.push(build_document_summary_memory_request(
                document,
                chunks.len(),
                &document_search_text,
                document_summary_source,
            ));
        }

        requests
    }

    async fn build_document_search_text_for_ingest(
        &self,
        document: &StoredDocument,
        chunks: &[Chunk],
    ) -> (String, &'static str) {
        if let Some(summary_llm) = &self.config.summary_llm {
            let content = build_llm_document_summary_source(chunks);
            match summary_llm
                .summarize_document(&document.name, &content)
                .await
            {
                Ok(summary) => {
                    let summary_lines = document_summary_lines_from_text(&summary);
                    if !summary_lines.is_empty() {
                        return (
                            build_document_search_text_from_summary_lines(document, &summary_lines),
                            "llm",
                        );
                    }
                    eprintln!(
                        "summary LLM returned no usable summary for {}; using offline document summary",
                        document.name
                    );
                }
                Err(error) => {
                    eprintln!(
                        "summary LLM failed for {}: {error}; using offline document summary",
                        document.name
                    );
                }
            }
        }

        // TODO: 后续需要把这里升级为更通用的 document_profile 生成逻辑，
        // 包含章节大纲、关键词、技术标识符和代表句；当前固定信号词摘要只作为无 LLM
        // 环境下的临时离线兜底。
        (build_document_search_text(document, chunks), "offline")
    }

    fn expand_retrieved_documents(
        &self,
        dataset_id: &str,
        results: Vec<ScoredMemory>,
        top_k: usize,
    ) -> Result<Vec<SearchChunk>> {
        let mut chunks = Vec::new();
        let mut seen_chunks = HashSet::new();
        let mut expanded_documents = HashSet::new();

        for scored in results {
            if chunks.len() >= top_k {
                break;
            }

            let metadata = &scored.record.metadata;
            let Some(document_id) = metadata_string(metadata, "document_id") else {
                continue;
            };
            if !expanded_documents.insert(document_id.clone()) {
                continue;
            }

            let document = match self.repo.get_stored_document(&document_id)? {
                Some(document) if document.dataset_id == dataset_id => document,
                _ => continue,
            };
            let preferred_chunk_id = match metadata_string(metadata, "record_kind").as_deref() {
                Some(DOCUMENT_SUMMARY_RECORD_KIND) => None,
                _ => metadata_string(metadata, "chunk_id"),
            };
            let document_chunks = self.repo.list_chunks(dataset_id, &document_id)?;
            append_document_chunks(
                &mut chunks,
                &mut seen_chunks,
                &document,
                &document_chunks,
                preferred_chunk_id.as_deref(),
                scored.score,
                top_k,
            );
        }

        Ok(chunks)
    }

    fn ensure_dataset_exists(&self, dataset_id: &str) -> Result<()> {
        anyhow::ensure!(
            self.repo.get_dataset(dataset_id)?.is_some(),
            "dataset not found"
        );
        Ok(())
    }
}

fn build_chunks_from_file(document: &StoredDocument, config: &RagConfig) -> Result<Vec<Chunk>> {
    let parse_result = ParserEngine::parse_file(
        &document.file_path,
        document.mime_type.as_deref(),
        Some(&document.name),
    )?;
    let parts = chunk_parse_result(
        parse_result,
        ChunkerConfig {
            max_tokens: config.chunk_size,
            token_counter: config.token_counter.clone(),
        },
    )?;
    let now = current_time_ms();
    Ok(parts
        .into_iter()
        .enumerate()
        .map(|(index, part)| Chunk {
            id: format!("{}_chunk_{}", document.id, index),
            dataset_id: document.dataset_id.clone(),
            document_id: document.id.clone(),
            chunk_index: index,
            content: part.content,
            chunk_type: part.chunk_type.as_str().to_string(),
            token_count: part.token_count,
            parse_topology: part.parse_topology.as_str().to_string(),
            source_node_indices: part.source_node_indices,
            available: true,
            created_at_ms: now,
            updated_at_ms: now,
        })
        .collect())
}

fn build_chunk_memory_request(document: &StoredDocument, chunk: &Chunk) -> AddMemoryRequest {
    AddMemoryRequest {
        id: Some(chunk.id.clone()),
        text: build_search_index_text(&chunk.content),
        metadata: serde_json::json!({
            "memory_index_namespace": MEMORY_INDEX_NAMESPACE,
            "scope_id": &chunk.dataset_id,
            "dataset_id": &chunk.dataset_id,
            "document_id": &chunk.document_id,
            "chunk_id": &chunk.id,
            "chunk_index": chunk.chunk_index,
            "chunking_strategy": CHUNKING_STRATEGY,
            "record_kind": CHUNK_RECORD_KIND,
            "chunk_type": &chunk.chunk_type,
            "chunk_char_count": chunk.content.chars().count(),
            "chunk_token_count": chunk.token_count,
            "parse_topology": &chunk.parse_topology,
            "source_node_indices": &chunk.source_node_indices,
            "available": chunk.available,
            "source_name": &document.name,
            "source_path": &document.file_path,
        }),
    }
}

fn build_document_summary_memory_request(
    document: &StoredDocument,
    chunk_count: usize,
    document_search_text: &str,
    document_summary_source: &'static str,
) -> AddMemoryRequest {
    AddMemoryRequest {
        id: Some(document_summary_memory_id(&document.id)),
        text: build_search_index_text(document_search_text),
        metadata: serde_json::json!({
            "memory_index_namespace": MEMORY_INDEX_NAMESPACE,
            "scope_id": &document.dataset_id,
            "dataset_id": &document.dataset_id,
            "document_id": &document.id,
            "chunking_strategy": CHUNKING_STRATEGY,
            "record_kind": DOCUMENT_SUMMARY_RECORD_KIND,
            "document_summary_source": document_summary_source,
            "chunk_count": chunk_count,
            "available": false,
            "source_name": &document.name,
            "source_path": &document.file_path,
        }),
    }
}

fn document_memory_delete_filters(dataset_id: &str, document_id: &str) -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "memory_index_namespace": MEMORY_INDEX_NAMESPACE,
            "dataset_id": dataset_id,
            "document_id": document_id,
        }),
        serde_json::json!({
            "chunking_strategy": CHUNKING_STRATEGY,
            "dataset_id": dataset_id,
            "document_id": document_id,
        }),
    ]
}

fn safe_file_name(file_name: &str) -> Result<String> {
    let file_name = Path::new(file_name)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .trim();
    anyhow::ensure!(!file_name.is_empty(), "file name must not be empty");
    Ok(file_name.to_string())
}

async fn remove_replaced_file(old_file_path: &str, new_file_path: &str) -> Result<()> {
    if old_file_path == new_file_path {
        return Ok(());
    }
    match fs::remove_file(old_file_path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to remove replaced document file {old_file_path}")),
    }
}

async fn remove_document_file_dir(file_path: &str) -> Result<()> {
    let file_path = Path::new(file_path);
    let Some(document_dir) = file_path.parent() else {
        return match fs::remove_file(file_path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error)
                .with_context(|| format!("failed to remove document file {}", file_path.display())),
        };
    };

    match fs::remove_dir_all(document_dir).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to remove document file directory {}",
                document_dir.display()
            )
        }),
    }
}

fn metadata_string(metadata: &serde_json::Value, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn document_recall_candidate_top_k(top_k: usize) -> usize {
    top_k
        .saturating_mul(DOCUMENT_RECALL_MULTIPLIER)
        .max(DOCUMENT_RECALL_MIN_CANDIDATES)
}

fn document_summary_memory_id(document_id: &str) -> String {
    format!("{document_id}_document_summary")
}

fn build_document_search_text(document: &StoredDocument, chunks: &[Chunk]) -> String {
    let body_summary = build_document_body_summary(chunks);
    build_document_search_text_from_summary_lines(document, &body_summary)
}

fn build_document_search_text_from_summary_lines(
    document: &StoredDocument,
    body_summary: &[String],
) -> String {
    let title = document_title(&document.name);
    let mut text = String::new();
    push_limited_line(
        &mut text,
        &format!("文档标题: {}", document.name.trim()),
        DOCUMENT_SUMMARY_MAX_CHARS,
    );
    if title != document.name.trim() {
        push_limited_line(
            &mut text,
            &format!("文档主题: {title}"),
            DOCUMENT_SUMMARY_MAX_CHARS,
        );
    }
    if !body_summary.is_empty() {
        push_limited_line(&mut text, "正文摘要:", DOCUMENT_SUMMARY_MAX_CHARS);
    }
    for (index, line) in body_summary.iter().enumerate() {
        push_limited_line(
            &mut text,
            &format!("摘要{}: {line}", index),
            DOCUMENT_SUMMARY_MAX_CHARS,
        );
    }
    text.trim().to_string()
}

fn build_llm_document_summary_source(chunks: &[Chunk]) -> String {
    let mut text = String::new();
    for chunk in chunks {
        push_limited_line(
            &mut text,
            &format!("片段{}:\n{}", chunk.chunk_index, chunk.content.trim()),
            LLM_DOCUMENT_SUMMARY_SOURCE_MAX_CHARS,
        );
    }
    text.trim().to_string()
}

fn document_summary_lines_from_text(text: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut order = 0;
    for text in document_summary_candidate_texts(text) {
        let Some(text) = normalize_document_summary_candidate(&text) else {
            continue;
        };
        candidates.push(DocumentSummaryCandidate {
            order,
            score: 0,
            text,
        });
        order += 1;
    }

    select_document_summary_candidates(&candidates, false)
        .into_iter()
        .map(|candidate| candidate.text)
        .collect()
}

#[derive(Clone, Debug)]
struct DocumentSummaryCandidate {
    order: usize,
    score: i32,
    text: String,
}

fn build_document_body_summary(chunks: &[Chunk]) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut order = 0;
    for chunk in chunks {
        for text in document_summary_candidate_texts(&chunk.content) {
            let Some(text) = normalize_document_summary_candidate(&text) else {
                continue;
            };
            let score = document_summary_candidate_score(&text, chunk.chunk_index);
            candidates.push(DocumentSummaryCandidate { order, score, text });
            order += 1;
        }
    }

    let selected = select_document_summary_candidates(&candidates, true);
    let selected = if selected.is_empty() {
        select_document_summary_candidates(&candidates, false)
    } else {
        selected
    };

    selected
        .into_iter()
        .map(|candidate| candidate.text)
        .collect()
}

fn document_summary_candidate_texts(content: &str) -> Vec<String> {
    let mut texts = Vec::new();
    for paragraph in content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if paragraph.chars().count() <= DOCUMENT_SUMMARY_CANDIDATE_MAX_CHARS {
            texts.push(paragraph.to_string());
            continue;
        }
        texts.extend(split_document_summary_sentences(paragraph));
    }
    if texts.is_empty() && !content.trim().is_empty() {
        texts.push(content.trim().to_string());
    }
    texts
}

fn split_document_summary_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        current.push(character);
        if matches!(character, '。' | '！' | '？' | '；' | '!' | '?' | ';') {
            let sentence = current.trim();
            if !sentence.is_empty() {
                sentences.push(sentence.to_string());
            }
            current.clear();
        }
    }
    let tail = current.trim();
    if !tail.is_empty() {
        sentences.push(tail.to_string());
    }
    sentences
}

fn normalize_document_summary_candidate(text: &str) -> Option<String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = text.trim();
    if text.chars().count() < 2 {
        return None;
    }
    Some(truncate_chars(text, DOCUMENT_SUMMARY_CANDIDATE_MAX_CHARS))
}

fn document_summary_candidate_score(text: &str, chunk_index: usize) -> i32 {
    let mut score = 0;
    for term in DOCUMENT_SUMMARY_SIGNAL_TERMS {
        if text.contains(term) {
            score += 6;
        }
    }
    if looks_like_document_summary_heading(text) {
        score += 8;
    }
    let chars = text.chars().count();
    if chars <= 80 {
        score += 3;
    } else if chars > DOCUMENT_SUMMARY_CANDIDATE_MAX_CHARS {
        score -= 4;
    }
    if chunk_index == 0 {
        score += 1;
    }
    score
}

fn looks_like_document_summary_heading(text: &str) -> bool {
    let chars = text.chars().count();
    chars <= 40
        && !text
            .chars()
            .any(|character| matches!(character, '。' | '！' | '？' | '；' | '.' | '!' | '?' | ';'))
}

fn select_document_summary_candidates(
    candidates: &[DocumentSummaryCandidate],
    require_signal: bool,
) -> Vec<DocumentSummaryCandidate> {
    let mut ranked = candidates
        .iter()
        .filter(|candidate| !require_signal || candidate.score > 0)
        .cloned()
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.order.cmp(&right.order))
    });

    let mut selected = Vec::new();
    let mut seen = HashSet::new();
    let mut total_chars = 0;
    for candidate in ranked {
        if selected.len() >= DOCUMENT_SUMMARY_MAX_LINES {
            break;
        }
        if !seen.insert(candidate.text.clone()) {
            continue;
        }
        let candidate_chars = candidate.text.chars().count();
        if total_chars > 0 && total_chars + candidate_chars > DOCUMENT_SUMMARY_BODY_MAX_CHARS {
            continue;
        }
        total_chars += candidate_chars;
        selected.push(candidate);
    }

    selected.sort_by_key(|candidate| candidate.order);
    selected
}

fn build_search_index_text(text: &str) -> String {
    let normalized = normalize_search_text(text);
    let tokens = lexical_tokens(&normalized);
    search_text_from_tokens(&normalized, tokens)
}

fn build_search_query_text(text: &str) -> String {
    let normalized = normalize_search_text(text);
    let tokens = lexical_tokens(&normalized);
    let ascii_terms = ascii_identifier_terms_from_tokens(&tokens);
    if ascii_terms.len() >= MIN_ASCII_IDENTIFIER_QUERY_TERMS {
        ascii_terms.join(" ")
    } else {
        search_text_from_tokens(&normalized, tokens)
    }
}

fn search_text_from_tokens(normalized: &str, tokens: Vec<String>) -> String {
    if tokens.is_empty() {
        normalized.trim().to_string()
    } else {
        tokens.join(" ")
    }
}

fn normalize_search_text(text: &str) -> String {
    let mut normalized = text.trim().to_string();
    for (from, to) in SEARCH_TEXT_REPLACEMENTS {
        normalized = normalized.replace(from, to);
    }
    normalized
}

fn filter_unrelated_results(query: &str, results: Vec<ScoredMemory>) -> Vec<ScoredMemory> {
    let query_terms = retrieval_terms(query);
    let required_ascii_terms = required_ascii_overlap_terms(query);
    if query_terms.is_empty() {
        return results;
    }

    results
        .into_iter()
        .filter(|scored| {
            has_retrieval_overlap(&query_terms, &required_ascii_terms, &scored.record.text)
        })
        .collect()
}

fn filter_low_relevance_results(results: Vec<ScoredMemory>) -> Vec<ScoredMemory> {
    results
        .into_iter()
        .filter(|scored| scored.score >= MIN_RELEVANCE_SCORE)
        .collect()
}

fn has_retrieval_overlap(
    query_terms: &HashSet<String>,
    required_ascii_terms: &HashSet<String>,
    candidate_text: &str,
) -> bool {
    let candidate_terms = retrieval_terms(candidate_text);
    if !required_ascii_terms.is_empty() {
        let overlap_count = required_ascii_terms
            .iter()
            .filter(|term| candidate_terms.contains(*term))
            .count();
        return overlap_count >= required_ascii_overlap_count(required_ascii_terms.len());
    }

    candidate_terms
        .into_iter()
        .any(|term| query_terms.contains(&term))
}

fn retrieval_terms(text: &str) -> HashSet<String> {
    let normalized = normalize_search_text(text);
    lexical_tokens(&normalized)
        .into_iter()
        .filter(|term| term.chars().count() >= 2)
        .filter(|term| !is_search_stop_term(term))
        .collect()
}

fn required_ascii_overlap_terms(text: &str) -> HashSet<String> {
    let normalized = normalize_search_text(text);
    let tokens = lexical_tokens(&normalized);
    let terms = ascii_identifier_terms_from_tokens(&tokens);
    if terms.len() < MIN_ASCII_IDENTIFIER_QUERY_TERMS {
        return HashSet::new();
    }
    terms.into_iter().collect()
}

fn ascii_identifier_terms_from_tokens(tokens: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    tokens
        .iter()
        .filter(|term| is_ascii_identifier_term(term))
        .filter(|term| !is_search_stop_term(term))
        .filter_map(|term| {
            if seen.insert(term.clone()) {
                Some(term.clone())
            } else {
                None
            }
        })
        .collect()
}

fn is_ascii_identifier_term(term: &str) -> bool {
    term.chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '_')
        && term
            .chars()
            .any(|character| character.is_ascii_alphabetic())
}

fn required_ascii_overlap_count(term_count: usize) -> usize {
    MIN_ASCII_IDENTIFIER_OVERLAP_TERMS.min(term_count)
}

fn is_search_stop_term(term: &str) -> bool {
    matches!(
        term,
        "为什么"
            | "怎么办"
            | "怎么"
            | "什么"
            | "哪些"
            | "可能"
            | "原因"
            | "问题"
            | "处理"
            | "修复"
            | "排查"
            | "故障"
            | "异常"
            | "业务"
            | "系统"
            | "一直"
            | "失败"
    )
}

fn lexical_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut segment = Vec::new();
    let mut ascii_run = String::new();

    for character in text.chars() {
        if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
            ascii_run.push(character.to_ascii_lowercase());
            continue;
        }

        flush_ascii_run(&mut ascii_run, &mut segment);
        if character.is_alphanumeric() {
            segment.push(character.to_string());
        } else {
            flush_lexical_segment(&mut segment, &mut tokens);
        }
    }

    flush_ascii_run(&mut ascii_run, &mut segment);
    flush_lexical_segment(&mut segment, &mut tokens);
    tokens
}

fn flush_ascii_run(ascii_run: &mut String, segment: &mut Vec<String>) {
    if ascii_run.is_empty() {
        return;
    }
    segment.push(std::mem::take(ascii_run));
}

fn flush_lexical_segment(segment: &mut Vec<String>, tokens: &mut Vec<String>) {
    if segment.is_empty() {
        return;
    }

    tokens.extend(segment.iter().cloned());
    for window_size in 2..=3 {
        if segment.len() < window_size {
            continue;
        }
        for window in segment.windows(window_size) {
            tokens.push(window.concat());
        }
    }
    segment.clear();
}

fn document_title(name: &str) -> String {
    Path::new(name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(name)
        .trim()
        .to_string()
}

fn push_limited_line(text: &mut String, line: &str, max_chars: usize) {
    let current_chars = text.chars().count();
    if current_chars >= max_chars {
        return;
    }
    if !text.is_empty() {
        push_limited_text(text, "\n", max_chars);
    }
    push_limited_text(text, line, max_chars);
}

fn push_limited_text(text: &mut String, value: &str, max_chars: usize) {
    let current_chars = text.chars().count();
    if current_chars >= max_chars {
        return;
    }
    let remaining = max_chars - current_chars;
    text.extend(value.chars().take(remaining));
}

fn append_document_chunks(
    output: &mut Vec<SearchChunk>,
    seen_chunks: &mut HashSet<String>,
    document: &StoredDocument,
    chunks: &[Chunk],
    preferred_chunk_id: Option<&str>,
    base_score: f32,
    top_k: usize,
) {
    let ordered_indices = ordered_chunk_indices(chunks, preferred_chunk_id);
    for (rank, index) in ordered_indices.into_iter().enumerate() {
        if output.len() >= top_k {
            break;
        }
        let Some(chunk) = chunks.get(index) else {
            continue;
        };
        if !chunk.available || !seen_chunks.insert(chunk.id.clone()) {
            continue;
        }
        let score = expanded_chunk_score(base_score, rank);
        if score < MIN_RELEVANCE_SCORE {
            break;
        }
        output.push(SearchChunk {
            chunk_id: chunk.id.clone(),
            dataset_id: chunk.dataset_id.clone(),
            document_id: chunk.document_id.clone(),
            source_name: Some(document.name.clone()),
            source_path: Some(document.file_path.clone()),
            content: chunk.content.clone(),
            score,
        });
    }
}

fn ordered_chunk_indices(chunks: &[Chunk], preferred_chunk_id: Option<&str>) -> Vec<usize> {
    let preferred_index = preferred_chunk_id
        .and_then(|chunk_id| chunks.iter().position(|chunk| chunk.id == chunk_id));
    let mut indices = Vec::with_capacity(chunks.len());
    if let Some(index) = preferred_index {
        indices.push(index);
    }
    for index in 0..chunks.len() {
        if Some(index) != preferred_index {
            indices.push(index);
        }
    }
    indices
}

fn expanded_chunk_score(base_score: f32, rank: usize) -> f32 {
    (base_score - (rank as f32 * 0.001)).max(0.0)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut truncated = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        truncated.push_str("...");
    }
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_core::{
        HashEmbedding, MemoryRecord, RetrievalConfig, SearchMode, SqliteMemoryStore,
    };

    use crate::token_counter::TestTokenCounter;

    fn chunk(id: &str, index: usize, content: &str) -> Chunk {
        Chunk {
            id: id.to_string(),
            dataset_id: "dataset".to_string(),
            document_id: "document".to_string(),
            chunk_index: index,
            content: content.to_string(),
            chunk_type: "text".to_string(),
            token_count: content.chars().count(),
            parse_topology: "list".to_string(),
            source_node_indices: vec![index],
            available: true,
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    fn scored_memory(id: &str, text: &str, score: f32) -> ScoredMemory {
        ScoredMemory {
            record: MemoryRecord {
                id: id.to_string(),
                text: text.to_string(),
                metadata: serde_json::json!({}),
                embedding: Vec::new(),
                created_at_ms: 1,
                updated_at_ms: 1,
            },
            score,
        }
    }

    fn test_service() -> (RagService, tempfile::TempDir) {
        test_service_with_chunk_size(512)
    }

    fn test_service_with_chunk_size(chunk_size: usize) -> (RagService, tempfile::TempDir) {
        test_service_with_retrieval_config(
            chunk_size,
            16,
            RetrievalConfig {
                mode: SearchMode::Bm25,
                ..RetrievalConfig::default()
            },
        )
    }

    fn test_service_with_retrieval_config(
        chunk_size: usize,
        embedding_dimensions: usize,
        retrieval: RetrievalConfig,
    ) -> (RagService, tempfile::TempDir) {
        let temp = tempfile::tempdir().expect("tempdir");
        let rag_db_path = temp.path().join("memory-cases.sqlite");
        let memory_db_path = temp.path().join("memory-cases-index.sqlite");
        let repo = Arc::new(RagRepository::new(&rag_db_path));
        repo.initialize().expect("initialize repo");
        let store = Arc::new(SqliteMemoryStore::new(&memory_db_path));
        let embedder = Arc::new(HashEmbedding::new(embedding_dimensions));
        let memory = Arc::new(MemoryManager::with_retrieval_config(
            store, embedder, retrieval,
        ));
        let service = RagService::new(
            repo,
            memory,
            RagConfig {
                file_root: temp.path().join("files"),
                chunk_size,
                token_counter: Arc::new(TestTokenCounter),
                summary_llm: None,
            },
        );
        (service, temp)
    }

    async fn create_dataset_and_ingest_document(
        service: &RagService,
        document_id: &str,
        task_id: &str,
        name: &str,
        content: &str,
    ) {
        service
            .create_dataset(CreateDatasetRequest {
                id: Some("dataset-1".to_string()),
                name: "Dataset".to_string(),
                description: None,
            })
            .expect("create dataset");
        ingest_document(service, document_id, task_id, name, content).await;
    }

    async fn ingest_document(
        service: &RagService,
        document_id: &str,
        task_id: &str,
        name: &str,
        content: &str,
    ) {
        service
            .create_document(
                "dataset-1",
                CreateDocumentFileRequest {
                    id: Some(document_id.to_string()),
                    task_id: Some(task_id.to_string()),
                    name: name.to_string(),
                    file_name: name.to_string(),
                    mime_type: Some("text/plain".to_string()),
                    bytes: content.as_bytes().to_vec(),
                },
            )
            .await
            .expect("create document");
        assert!(service
            .run_next_ingestion_task()
            .await
            .expect("run ingestion task"));
    }

    async fn search_contents(service: &RagService, query: &str) -> Vec<String> {
        service
            .search_dataset(
                "dataset-1",
                SearchRequest {
                    query: query.to_string(),
                    top_k: 5,
                },
            )
            .await
            .expect("search dataset")
            .chunks
            .into_iter()
            .map(|chunk| chunk.content)
            .collect()
    }

    #[tokio::test]
    async fn update_document_replaces_chunks_and_search_records() {
        let (service, _temp) = test_service();
        create_dataset_and_ingest_document(
            &service,
            "doc-1",
            "task-create",
            "old.txt",
            "oldneedle original guidance. Keep the legacy appliance online.",
        )
        .await;
        assert!(search_contents(&service, "oldneedle")
            .await
            .iter()
            .any(|content| content.contains("oldneedle")));

        let response = service
            .update_document(
                "dataset-1",
                "doc-1",
                UpdateDocumentFileRequest {
                    task_id: Some("task-update".to_string()),
                    name: Some("new.txt".to_string()),
                    file_name: Some("new.txt".to_string()),
                    mime_type: Some("text/plain".to_string()),
                    bytes: Some(
                        b"newneedle replacement guidance. Rotate the service token.".to_vec(),
                    ),
                },
            )
            .await
            .expect("update document");

        assert_eq!(response.document_id, "doc-1");
        assert_eq!(response.task_id, "task-update");
        let documents = service
            .list_documents("dataset-1")
            .expect("list documents")
            .documents;
        assert_eq!(documents[0].name, "new.txt");
        assert_eq!(documents[0].status, "uploaded");
        assert_eq!(documents[0].chunk_count, 0);
        assert!(service
            .list_chunks("dataset-1", "doc-1")
            .expect("list chunks after update")
            .chunks
            .is_empty());
        assert!(search_contents(&service, "oldneedle").await.is_empty());
        assert!(search_contents(&service, "newneedle").await.is_empty());

        assert!(service
            .run_next_ingestion_task()
            .await
            .expect("run update ingestion task"));
        assert!(search_contents(&service, "oldneedle").await.is_empty());
        assert!(search_contents(&service, "newneedle")
            .await
            .iter()
            .any(|content| content.contains("newneedle")));
        let documents = service
            .list_documents("dataset-1")
            .expect("list documents after ingest")
            .documents;
        assert_eq!(documents[0].status, "completed");
        assert!(documents[0].chunk_count > 0);
    }

    #[tokio::test]
    async fn delete_document_removes_chunks_search_records_tasks_and_files() {
        let (service, temp) = test_service();
        create_dataset_and_ingest_document(
            &service,
            "doc-1",
            "task-create",
            "delete-me.txt",
            "deleteneedle removable guidance. This content must disappear.",
        )
        .await;
        assert!(search_contents(&service, "deleteneedle")
            .await
            .iter()
            .any(|content| content.contains("deleteneedle")));
        let document_dir = temp.path().join("files").join("dataset-1").join("doc-1");
        assert!(document_dir.exists());

        let response = service
            .delete_document("dataset-1", "doc-1")
            .await
            .expect("delete document");

        assert_eq!(response.document_id, "doc-1");
        assert!(response.deleted);
        assert!(service
            .list_documents("dataset-1")
            .expect("list documents after delete")
            .documents
            .is_empty());
        assert!(service
            .list_chunks("dataset-1", "doc-1")
            .expect("list chunks after delete")
            .chunks
            .is_empty());
        assert!(service
            .get_task("task-create")
            .expect("get deleted task")
            .is_none());
        assert!(search_contents(&service, "deleteneedle").await.is_empty());
        assert!(!document_dir.exists());
    }

    #[tokio::test]
    async fn hybrid_search_handles_mixed_language_abbreviation_query() {
        let (service, _temp) = test_service_with_retrieval_config(
            512,
            256,
            RetrievalConfig {
                mode: SearchMode::Hybrid,
                ..RetrievalConfig::default()
            },
        );
        service
            .create_dataset(CreateDatasetRequest {
                id: Some("dataset-1".to_string()),
                name: "Dataset".to_string(),
                description: None,
            })
            .expect("create dataset");
        ingest_document(
            &service,
            "vpn-doc",
            "task-vpn",
            "VPN-MFA英文缩写.md",
            r#"### Remote access notes

The user can open normal intranet pages, but VPN login keeps looping back to the approval screen. The client displays "MFA token expired" and then asks for 2FA again.

The root cause is phone time drift after the user disabled automatic date and time. The authenticator code is generated with the wrong clock, so the VPN gateway rejects every one-time password.

Fix steps: sync phone time automatically, remove the old authenticator binding, ask the admin to reset the MFA seed, then log in to VPN again and approve the push notification."#,
        )
        .await;
        ingest_document(
            &service,
            "ssh-doc",
            "task-ssh",
            "终端记录-SSH主机指纹变更.txt",
            "终端记录：SSH 登录提示主机指纹变更\n\n运维从跳板机登录 build-node-17 时，ssh 直接中断并提示 WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED。处置时先核对机器身份，再执行 ssh-keygen -R build-node-17 删除旧指纹。",
        )
        .await;

        let response = service
            .search_dataset(
                "dataset-1",
                SearchRequest {
                    query: "VPN 登录一直让 2FA，提示 MFA token expired，怎么处理？".to_string(),
                    top_k: 2,
                },
            )
            .await
            .expect("search dataset");

        assert!(!response.chunks.is_empty());
        assert!(response
            .chunks
            .iter()
            .all(|chunk| chunk.source_name.as_deref() == Some("VPN-MFA英文缩写.md")));
        assert!(response
            .chunks
            .iter()
            .any(|chunk| chunk.content.contains("MFA token expired")));
    }

    #[tokio::test]
    async fn chat_answer_previews_all_returned_chunks() {
        let (service, _temp) = test_service_with_chunk_size(4);
        create_dataset_and_ingest_document(
            &service,
            "doc-1",
            "task-create",
            "multi.txt",
            "aa first. aa second. aa third. aa fourth. aa fifth.",
        )
        .await;

        let response = service
            .chat_completion(crate::model::ChatCompletionRequest {
                dataset_id: "dataset-1".to_string(),
                question: "aa".to_string(),
                top_k: 5,
            })
            .await
            .expect("chat completion");

        assert_eq!(response.references.len(), 5);
        assert!(response.answer.contains("Found 5 relevant chunks"));
        assert!(response.answer.contains("aa fourth"));
        assert!(response.answer.contains("aa fifth"));
    }

    #[test]
    fn document_summary_keeps_title_topic_and_body_summary_without_question_bridges() {
        let document = StoredDocument {
            id: "document".to_string(),
            dataset_id: "dataset".to_string(),
            name: "101号CPU核因栈空间不足导致系统挂死.md".to_string(),
            file_path: "/tmp/101号CPU核因栈空间不足导致系统挂死.md".to_string(),
            mime_type: Some("text/markdown".to_string()),
        };
        let chunks = vec![chunk(
            "document_chunk_0",
            0,
            "问题根因\n\n栈空间不足，触发 Insufficient stack space。",
        )];

        let summary = build_document_search_text(&document, &chunks);

        assert!(summary.contains("文档标题: 101号CPU核因栈空间不足导致系统挂死.md"));
        assert!(summary.contains("文档主题: 101号CPU核因栈空间不足导致系统挂死"));
        assert!(summary.contains("问题根因"));
        assert!(!summary.contains("相关问法:"));
        assert!(!summary.contains("101号CPU核为什么会导致系统挂死？"));
    }

    #[test]
    fn document_summary_uses_compact_body_summary() {
        let document = StoredDocument {
            id: "document".to_string(),
            dataset_id: "dataset".to_string(),
            name: "苹果手机中软件安装方法.txt".to_string(),
            file_path: "/tmp/苹果手机中软件安装方法.txt".to_string(),
            mime_type: Some("text/plain".to_string()),
        };
        let chunks = vec![
            chunk(
                "document_chunk_0",
                0,
                "背景说明\n\n这是一段很长的铺垫文字，没有直接说明操作步骤，只是描述资料来源和适用范围。",
            ),
            chunk(
                "document_chunk_1",
                1,
                "处理方案\n\n打开 App Store，搜索要安装的软件，点击获取并完成验证。",
            ),
        ];

        let summary = build_document_search_text(&document, &chunks);

        assert!(summary.contains("文档标题: 苹果手机中软件安装方法.txt"));
        assert!(summary.contains("正文摘要:"));
        assert!(summary.contains("处理方案"));
        assert!(summary.contains("点击获取"));
        assert!(!summary.contains("文档内容:"));
        assert!(!summary.contains("片段0:"));
    }

    #[test]
    fn document_search_text_can_use_llm_summary_lines() {
        let document = StoredDocument {
            id: "document".to_string(),
            dataset_id: "dataset".to_string(),
            name: "D17-网关日志-错误码.log".to_string(),
            file_path: "/tmp/D17-网关日志-错误码.log".to_string(),
            mime_type: Some("text/plain".to_string()),
        };
        let summary_lines = document_summary_lines_from_text(
            "gateway 到 payment-api 出现 tls handshake timeout。\n临时处理是 rotate gateway certificate bundle 并 restart envoy sidecar。",
        );

        let summary = build_document_search_text_from_summary_lines(&document, &summary_lines);

        assert!(summary.contains("文档标题: D17-网关日志-错误码.log"));
        assert!(summary.contains("文档主题: D17-网关日志-错误码"));
        assert!(summary.contains("正文摘要:"));
        assert!(summary.contains("payment-api"));
        assert!(summary.contains("restart envoy sidecar"));
    }

    #[test]
    fn llm_document_summary_source_keeps_chunk_order() {
        let chunks = vec![
            chunk("document_chunk_0", 0, "第一段现象"),
            chunk("document_chunk_1", 1, "第二段处理方案"),
        ];

        let source = build_llm_document_summary_source(&chunks);

        assert!(source.contains("片段0:\n第一段现象"));
        assert!(source.contains("片段1:\n第二段处理方案"));
        assert!(
            source.find("片段0").expect("chunk 0 marker")
                < source.find("片段1").expect("chunk 1 marker")
        );
    }

    #[test]
    fn document_summary_falls_back_when_body_has_no_signal_terms() {
        let chunks = vec![chunk("document_chunk_0", 0, "alpha beta gamma")];

        let lines = build_document_body_summary(&chunks);

        assert_eq!(lines, vec!["alpha beta gamma"]);
    }

    #[test]
    fn search_index_text_tokenizes_cjk_and_ascii_runs() {
        let index_text = build_search_index_text("101号CPU核为什么会导致系统挂死？");

        assert!(index_text.contains("101"));
        assert!(index_text.contains("cpu"));
        assert!(index_text.contains("系统"));
        assert!(index_text.contains("挂死"));
        assert!(index_text.contains("101号cpu"));
    }

    #[test]
    fn search_index_text_normalizes_common_typos() {
        let index_text = build_search_index_text("Windows 上蓝芽键盘一直转圈");

        assert!(index_text.contains("蓝牙"));
        assert!(index_text.contains("蓝牙键"));
        assert!(!index_text.contains("蓝芽"));
    }

    #[test]
    fn search_query_text_prefers_ascii_identifiers_for_mixed_abbreviations() {
        let query_text =
            build_search_query_text("VPN 登录一直让 2FA，提示 MFA token expired，怎么处理？");

        assert_eq!(query_text, "vpn 2fa mfa token expired");
    }

    #[test]
    fn retrieval_overlap_requires_identifier_match_for_mixed_abbreviations() {
        let query = "VPN 登录一直让 2FA，提示 MFA token expired，怎么处理？";
        let query_terms = retrieval_terms(query);
        let required_ascii_terms = required_ascii_overlap_terms(query);

        assert!(required_ascii_terms.contains("vpn"));
        assert!(required_ascii_terms.contains("mfa"));
        assert!(has_retrieval_overlap(
            &query_terms,
            &required_ascii_terms,
            "The client displays MFA token expired and then asks for 2FA again."
        ));
        assert!(!has_retrieval_overlap(
            &query_terms,
            &required_ascii_terms,
            "终端记录：SSH 登录提示主机指纹变更，Host key verification failed."
        ));
    }

    #[test]
    fn retrieval_overlap_requires_multiple_identifier_matches_for_numeric_clues() {
        let query = "路由器发烫后 5GHz 从 866Mbps 降到 72Mbps，Wi-Fi 变慢怎么办？";
        let query_terms = retrieval_terms(query);
        let required_ascii_terms = required_ascii_overlap_terms(query);

        assert!(required_ascii_terms.contains("5ghz"));
        assert!(required_ascii_terms.contains("866mbps"));
        assert!(required_ascii_terms.contains("72mbps"));
        assert!(has_retrieval_overlap(
            &query_terms,
            &required_ascii_terms,
            "管理页显示 5GHz 协商速率从 866Mbps 降到 72Mbps，根因是路由器过热降频。"
        ));
        assert!(!has_retrieval_overlap(
            &query_terms,
            &required_ascii_terms,
            "家里 Wi-Fi 变慢：路由器信道拥挤。优先连接 5GHz Wi-Fi。"
        ));
    }

    #[test]
    fn retrieval_overlap_filters_unrelated_candidates() {
        let query = "古典吉他琴弦张力和指板保养方法";
        let query_terms = retrieval_terms(query);
        let required_ascii_terms = required_ascii_overlap_terms(query);

        assert!(!has_retrieval_overlap(
            &query_terms,
            &required_ascii_terms,
            "数据库请求偶发超时，根因是 NUMA 本地回收和 direct reclaim。"
        ));
    }

    #[test]
    fn retrieval_overlap_keeps_bluetooth_typo_candidate() {
        let query = "Windows 上蓝芽键盘一直转圈，配对失败怎么办？";
        let query_terms = retrieval_terms(query);
        let required_ascii_terms = required_ascii_overlap_terms(query);

        assert!(has_retrieval_overlap(
            &query_terms,
            &required_ascii_terms,
            "客户说新换的蓝牙键盘在 Windows 上能看到设备名，但点击连接后一直转圈。"
        ));
    }

    #[test]
    fn retrieval_overlap_filters_generic_failure_only_candidates() {
        let query = "Windows 上蓝芽键盘一直转圈，配对失败怎么办？";
        let query_terms = retrieval_terms(query);
        let required_ascii_terms = required_ascii_overlap_terms(query);

        assert!(!has_retrieval_overlap(
            &query_terms,
            &required_ascii_terms,
            "文档标题: IRQ 352 亲和性修改失败\n业务侧希望把该中断固定到 NUMA node1，但手动写入一直不生效。"
        ));
    }

    #[test]
    fn retrieval_overlap_keeps_weakly_related_symptom_candidates() {
        let query = "数据库请求为什么偶发超时，CPU和磁盘都不高？";
        let query_terms = retrieval_terms(query);
        let required_ascii_terms = required_ascii_overlap_terms(query);

        assert!(has_retrieval_overlap(
            &query_terms,
            &required_ascii_terms,
            "文档标题: 数据库因NUMA本地回收导致响应超时\n问题现象: SQL 执行超过阈值。"
        ));
    }

    #[test]
    fn relevance_threshold_returns_at_most_top_k_without_filling_low_scores() {
        let results = filter_low_relevance_results(vec![
            scored_memory("strong", "扫描仪 ADF 自动进纸歪斜", 0.7),
            scored_memory("edge", "扫描仪 走纸传感器校准", MIN_RELEVANCE_SCORE),
            scored_memory("weak", "蓝牙键盘配对失败", MIN_RELEVANCE_SCORE - 0.001),
        ]);

        assert_eq!(
            results
                .iter()
                .map(|scored| scored.record.id.as_str())
                .collect::<Vec<_>>(),
            vec!["strong", "edge"]
        );
    }

    #[test]
    fn expanded_chunks_do_not_return_scores_below_relevance_threshold() {
        let document = StoredDocument {
            id: "document".to_string(),
            dataset_id: "dataset".to_string(),
            name: "scanner.txt".to_string(),
            file_path: "/tmp/scanner.txt".to_string(),
            mime_type: Some("text/plain".to_string()),
        };
        let chunks = vec![
            chunk("document_chunk_0", 0, "扫描仪 ADF 自动进纸歪斜"),
            chunk("document_chunk_1", 1, "走纸传感器校准"),
        ];
        let mut output = Vec::new();
        let mut seen_chunks = HashSet::new();

        append_document_chunks(
            &mut output,
            &mut seen_chunks,
            &document,
            &chunks,
            None,
            MIN_RELEVANCE_SCORE,
            2,
        );

        assert_eq!(output.len(), 1);
        assert_eq!(output[0].chunk_id, "document_chunk_0");
        assert_eq!(output[0].score, MIN_RELEVANCE_SCORE);
    }

    #[test]
    fn ordered_chunk_indices_prefers_memory_hit_then_document_order() {
        let chunks = vec![
            chunk("document_chunk_0", 0, "root cause"),
            chunk("document_chunk_1", 1, "solution"),
            chunk("document_chunk_2", 2, "appendix"),
        ];

        assert_eq!(
            ordered_chunk_indices(&chunks, Some("document_chunk_1")),
            vec![1, 0, 2]
        );
        assert_eq!(ordered_chunk_indices(&chunks, None), vec![0, 1, 2]);
    }
}
