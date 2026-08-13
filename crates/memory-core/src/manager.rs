use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use uuid::Uuid;

use crate::record::{extract_scope_id, extract_scope_id_from_filter, metadata_matches};
use crate::{
    cosine_similarity, AddMemoryRequest, AddMemoryResponse, EmbeddingProvider,
    GraphRetrieveContextRequest, MemoryError, MemoryRecord, MemoryResult, MemoryStore, Reranker,
    RetrievalConfig, ScoredMemory, SearchMemoryRequest, SearchMode, SqliteMemoryStore,
};
use crate::{
    graph::{EvidenceRecordContextUnit, FactContextUnit, GraphMemoryRecord},
    sqlite::GraphRepository,
};

const DEFAULT_EMBEDDING_BATCH_SIZE: usize = 64;
const EMBEDDING_PROFILE_METADATA_KEY: &str = "memory_core_embedding_profile";
const GRAPH_FACTS_METADATA_KEY: &str = "graph_facts";
const GRAPH_FACTS_TRUNCATED_METADATA_KEY: &str = "graph_facts_truncated";
const GRAPH_MATCHES_METADATA_KEY: &str = "graph_matches";
const MAX_GRAPH_FACTS_PER_RECORD: usize = 16;
const MAX_GRAPH_FACT_METADATA_BYTES: usize = 16 * 1024;

#[async_trait]
pub trait LongTermMemory: Send + Sync {
    async fn add(&self, request: AddMemoryRequest) -> MemoryResult<AddMemoryResponse>;
    async fn search(&self, request: SearchMemoryRequest) -> MemoryResult<Vec<ScoredMemory>>;
}

pub struct MemoryManager {
    store: Arc<dyn MemoryStore>,
    embedder: Arc<dyn EmbeddingProvider>,
    retrieval_config: RetrievalConfig,
    reranker: Option<Arc<dyn Reranker>>,
}

impl MemoryManager {
    pub fn new(store: Arc<dyn MemoryStore>, embedder: Arc<dyn EmbeddingProvider>) -> Self {
        Self::with_retrieval_config(store, embedder, RetrievalConfig::default())
    }

    pub fn with_retrieval_config(
        store: Arc<dyn MemoryStore>,
        embedder: Arc<dyn EmbeddingProvider>,
        retrieval_config: RetrievalConfig,
    ) -> Self {
        Self {
            store,
            embedder,
            retrieval_config,
            reranker: None,
        }
    }

    pub fn with_retrieval_config_and_reranker(
        store: Arc<dyn MemoryStore>,
        embedder: Arc<dyn EmbeddingProvider>,
        retrieval_config: RetrievalConfig,
        reranker: Arc<dyn Reranker>,
    ) -> Self {
        Self {
            store,
            embedder,
            retrieval_config,
            reranker: Some(reranker),
        }
    }

    pub async fn add_many(
        &self,
        requests: Vec<AddMemoryRequest>,
    ) -> MemoryResult<Vec<AddMemoryResponse>> {
        self.add_many_with_batch_size(requests, DEFAULT_EMBEDDING_BATCH_SIZE)
            .await
    }

    pub async fn add_many_with_batch_size(
        &self,
        requests: Vec<AddMemoryRequest>,
        batch_size: usize,
    ) -> MemoryResult<Vec<AddMemoryResponse>> {
        self.add_many_with_batch_size_and_progress(requests, batch_size, |_| {})
            .await
    }

    pub async fn add_many_with_batch_size_and_progress<F>(
        &self,
        requests: Vec<AddMemoryRequest>,
        batch_size: usize,
        mut on_embedded: F,
    ) -> MemoryResult<Vec<AddMemoryResponse>>
    where
        F: FnMut(usize),
    {
        let (responses, new_records) = self
            .build_records_with_batch_size_and_progress(requests, batch_size, &mut on_embedded)
            .await?;
        self.validate_new_record_embedding_profiles(&new_records)
            .await?;
        self.store.add_records(&new_records).await?;

        Ok(responses)
    }

    async fn build_records_with_batch_size_and_progress<F>(
        &self,
        requests: Vec<AddMemoryRequest>,
        batch_size: usize,
        on_embedded: &mut F,
    ) -> MemoryResult<(Vec<AddMemoryResponse>, Vec<MemoryRecord>)>
    where
        F: FnMut(usize),
    {
        if requests.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }

        let mut explicit_ids = HashSet::new();
        for request in &requests {
            if request.text.trim().is_empty() {
                return Err(MemoryError::InvalidInput {
                    message: "memory text must not be empty".to_string(),
                });
            }
            if let Some(id) = request.id.as_deref() {
                if !explicit_ids.insert(id) {
                    return Err(MemoryError::InvalidInput {
                        message: format!("duplicate memory id in batch: {id}"),
                    });
                }
            }
        }

        let batch_size = batch_size.max(1);
        let mut embeddings = Vec::with_capacity(requests.len());
        for chunk in requests.chunks(batch_size) {
            let texts = chunk
                .iter()
                .map(|request| request.text.trim().to_string())
                .collect::<Vec<_>>();
            embeddings.extend(self.embedder.embed(&texts).await?);
            on_embedded(chunk.len());
        }

        if embeddings.len() != requests.len() {
            return Err(MemoryError::Embedding {
                message: format!(
                    "embedding provider returned {} vectors for {} requests",
                    embeddings.len(),
                    requests.len()
                ),
            });
        }

        let now = current_time_ms();
        let embedding_profile = self.embedding_profile_metadata();
        let mut responses = Vec::with_capacity(requests.len());
        let mut records = Vec::with_capacity(requests.len());
        for (request, embedding) in requests.into_iter().zip(embeddings) {
            let id = request.id.unwrap_or_else(|| Uuid::new_v4().to_string());
            responses.push(AddMemoryResponse { id: id.clone() });
            records.push(MemoryRecord {
                id,
                text: request.text.trim().to_string(),
                metadata: metadata_with_embedding_profile(request.metadata, &embedding_profile),
                embedding,
                created_at_ms: now,
                updated_at_ms: now,
            });
        }

        Ok((responses, records))
    }

    pub async fn delete_by_filter(&self, filter: serde_json::Value) -> MemoryResult<usize> {
        self.delete_by_filters(vec![filter]).await
    }

    pub async fn delete_by_filters(&self, filters: Vec<serde_json::Value>) -> MemoryResult<usize> {
        if filters.is_empty() {
            return Ok(0);
        }
        let records = self.store.list_records().await?;
        let before = records.len();
        let retained = records
            .into_iter()
            .filter(|record| !metadata_matches_any(&record.metadata, &filters))
            .collect::<Vec<_>>();
        let deleted = before.saturating_sub(retained.len());
        if deleted > 0 {
            self.store.replace_all(&retained).await?;
        }
        Ok(deleted)
    }

    pub async fn search_many(
        &self,
        requests: Vec<SearchMemoryRequest>,
    ) -> MemoryResult<Vec<Vec<ScoredMemory>>> {
        self.search_many_with_batch_size(requests, DEFAULT_EMBEDDING_BATCH_SIZE)
            .await
    }

    pub async fn search_many_with_batch_size(
        &self,
        requests: Vec<SearchMemoryRequest>,
        batch_size: usize,
    ) -> MemoryResult<Vec<Vec<ScoredMemory>>> {
        self.search_many_with_batch_size_and_progress(requests, batch_size, |_| {})
            .await
    }

    pub async fn search_many_with_batch_size_and_progress<F>(
        &self,
        requests: Vec<SearchMemoryRequest>,
        batch_size: usize,
        mut on_embedded: F,
    ) -> MemoryResult<Vec<Vec<ScoredMemory>>>
    where
        F: FnMut(usize),
    {
        match self.retrieval_config.mode {
            SearchMode::Dense => {}
            SearchMode::Bm25 => {
                return self
                    .search_many_bm25_with_progress(requests, &mut on_embedded)
                    .await;
            }
            SearchMode::Graph => {
                return self
                    .search_many_graph_with_progress(requests, &mut on_embedded)
                    .await;
            }
            SearchMode::Hybrid => {
                return self
                    .search_many_hybrid_with_progress(requests, &mut on_embedded)
                    .await;
            }
        }

        if requests.is_empty() {
            return Ok(Vec::new());
        }

        for request in &requests {
            if request.query.trim().is_empty() {
                return Err(MemoryError::InvalidInput {
                    message: "search query must not be empty".to_string(),
                });
            }
        }

        let batch_size = batch_size.max(1);
        let mut query_embeddings = Vec::with_capacity(requests.len());
        for chunk in requests.chunks(batch_size) {
            let texts = chunk
                .iter()
                .map(|request| request.query.trim().to_string())
                .collect::<Vec<_>>();
            query_embeddings.extend(self.embedder.embed(&texts).await?);
            on_embedded(chunk.len());
        }

        if query_embeddings.len() != requests.len() {
            return Err(MemoryError::Embedding {
                message: format!(
                    "embedding provider returned {} vectors for {} search requests",
                    query_embeddings.len(),
                    requests.len()
                ),
            });
        }

        let mut all_results = Vec::with_capacity(requests.len());
        for (request, query_embedding) in requests.into_iter().zip(query_embeddings) {
            if request.top_k == 0 {
                all_results.push(Vec::new());
                continue;
            }

            let candidates = self
                .dense_candidates(&query_embedding, request.filter.as_ref(), request.top_k)
                .await?;
            all_results.push(
                self.fuse_optional_graph_channel(&request, candidates, request.top_k)
                    .await?,
            );
        }

        Ok(all_results)
    }

    async fn search_many_bm25_with_progress<F>(
        &self,
        requests: Vec<SearchMemoryRequest>,
        on_searched: &mut F,
    ) -> MemoryResult<Vec<Vec<ScoredMemory>>>
    where
        F: FnMut(usize),
    {
        let mut all_results = Vec::with_capacity(requests.len());
        for request in requests {
            all_results.push(self.search_bm25(request).await?);
            on_searched(1);
        }
        Ok(all_results)
    }

    async fn search_many_graph_with_progress<F>(
        &self,
        requests: Vec<SearchMemoryRequest>,
        on_searched: &mut F,
    ) -> MemoryResult<Vec<Vec<ScoredMemory>>>
    where
        F: FnMut(usize),
    {
        let mut all_results = Vec::with_capacity(requests.len());
        for request in requests {
            all_results.push(self.search_graph(request).await?);
            on_searched(1);
        }
        Ok(all_results)
    }

    async fn search_bm25(&self, request: SearchMemoryRequest) -> MemoryResult<Vec<ScoredMemory>> {
        if request.top_k == 0 {
            return Ok(Vec::new());
        }
        let query = request.query.trim();
        if query.is_empty() {
            return Err(MemoryError::InvalidInput {
                message: "search query must not be empty".to_string(),
            });
        }

        let sqlite_store = self.sqlite_store_for_mode(SearchMode::Bm25)?;
        let stage_started = Instant::now();
        tracing::info!(
            event = "ram_a.memory.search.stage.started",
            stage = "bm25_retrieve",
            candidate_limit = request.top_k
        );
        let candidates = sqlite_store
            .bm25_candidates(query, request.filter.as_ref(), request.top_k)
            .await?;
        tracing::info!(
            event = "ram_a.memory.search.stage.completed",
            stage = "bm25_retrieve",
            candidate_count = candidates.len(),
            elapsed_ms = stage_started.elapsed().as_millis() as u64
        );
        self.fuse_optional_graph_channel(&request, candidates, request.top_k)
            .await
    }

    async fn search_graph(&self, request: SearchMemoryRequest) -> MemoryResult<Vec<ScoredMemory>> {
        if request.top_k == 0 {
            return Ok(Vec::new());
        }
        if request.query.trim().is_empty() {
            return Err(MemoryError::InvalidInput {
                message: "search query must not be empty".to_string(),
            });
        }

        // This is an explicit evaluation/serving mode: do not fall back to raw-memory
        // candidates, so graph recall can be measured independently from dense/BM25.
        let stage_started = Instant::now();
        tracing::info!(
            event = "ram_a.memory.search.stage.started",
            stage = "graph_retrieve",
            candidate_limit = request.top_k
        );
        let candidates = self.graph_candidates(&request, request.top_k).await?;
        tracing::info!(
            event = "ram_a.memory.search.stage.completed",
            stage = "graph_retrieve",
            candidate_count = candidates.len(),
            elapsed_ms = stage_started.elapsed().as_millis() as u64
        );
        Ok(candidates)
    }

    async fn search_many_hybrid_with_progress<F>(
        &self,
        requests: Vec<SearchMemoryRequest>,
        on_embedded: &mut F,
    ) -> MemoryResult<Vec<Vec<ScoredMemory>>>
    where
        F: FnMut(usize),
    {
        let mut all_results = Vec::with_capacity(requests.len());
        for request in requests {
            all_results.push(
                self.search_hybrid_with_progress(request, on_embedded)
                    .await?,
            );
        }
        Ok(all_results)
    }

    async fn search_hybrid(&self, request: SearchMemoryRequest) -> MemoryResult<Vec<ScoredMemory>> {
        let mut noop = |_| {};
        self.search_hybrid_with_progress(request, &mut noop).await
    }

    async fn search_hybrid_with_progress<F>(
        &self,
        request: SearchMemoryRequest,
        on_embedded: &mut F,
    ) -> MemoryResult<Vec<ScoredMemory>>
    where
        F: FnMut(usize),
    {
        if request.top_k == 0 {
            return Ok(Vec::new());
        }
        let query = request.query.trim();
        if query.is_empty() {
            return Err(MemoryError::InvalidInput {
                message: "search query must not be empty".to_string(),
            });
        }

        let candidate_k = self.retrieval_config.candidate_limit(request.top_k);
        let mut stage_started = Instant::now();
        tracing::info!(
            event = "ram_a.memory.search.stage.started",
            stage = "query_embedding",
            provider = "openai_compatible",
            model = self.embedder.model_name(),
            dimensions = self.embedder.dimensions()
        );
        let query_embedding = self.embedder.embed_one(query).await?;
        tracing::info!(
            event = "ram_a.memory.search.stage.completed",
            stage = "query_embedding",
            dimensions = query_embedding.len(),
            elapsed_ms = stage_started.elapsed().as_millis() as u64
        );
        on_embedded(1);
        stage_started = Instant::now();
        tracing::info!(
            event = "ram_a.memory.search.stage.started",
            stage = "dense_retrieve",
            candidate_limit = candidate_k
        );
        let dense_candidates = self
            .dense_candidates(&query_embedding, request.filter.as_ref(), candidate_k)
            .await?;
        tracing::info!(
            event = "ram_a.memory.search.stage.completed",
            stage = "dense_retrieve",
            candidate_count = dense_candidates.len(),
            elapsed_ms = stage_started.elapsed().as_millis() as u64
        );
        let sqlite_store = self.sqlite_store_for_mode(SearchMode::Hybrid)?;
        stage_started = Instant::now();
        tracing::info!(
            event = "ram_a.memory.search.stage.started",
            stage = "bm25_retrieve",
            candidate_limit = candidate_k
        );
        let bm25_candidates = sqlite_store
            .bm25_candidates(query, request.filter.as_ref(), candidate_k)
            .await?;
        tracing::info!(
            event = "ram_a.memory.search.stage.completed",
            stage = "bm25_retrieve",
            candidate_count = bm25_candidates.len(),
            elapsed_ms = stage_started.elapsed().as_millis() as u64
        );

        let result_limit = if self.retrieval_config.rerank.enabled {
            self.retrieval_config.rerank.input_limit(request.top_k)
        } else {
            request.top_k
        };
        stage_started = Instant::now();
        tracing::info!(
            event = "ram_a.memory.search.stage.started",
            stage = "hybrid_fuse",
            embedding_weight = self.retrieval_config.embedding_weight,
            bm25_weight = self.retrieval_config.bm25_weight,
            result_limit
        );
        let candidates = fuse_hybrid_candidates(
            dense_candidates,
            bm25_candidates,
            result_limit,
            self.retrieval_config.embedding_weight,
            self.retrieval_config.bm25_weight,
        );
        tracing::info!(
            event = "ram_a.memory.search.stage.completed",
            stage = "hybrid_fuse",
            candidate_count = candidates.len(),
            elapsed_ms = stage_started.elapsed().as_millis() as u64
        );
        let candidates = if self.retrieval_config.graph.enabled {
            stage_started = Instant::now();
            tracing::info!(
                event = "ram_a.memory.search.stage.started",
                stage = "graph_augment",
                candidate_count = candidates.len(),
                result_limit
            );
            let candidates = self
                .fuse_optional_graph_channel(&request, candidates, result_limit)
                .await
                .inspect_err(|error| {
                    tracing::error!(
                        event = "ram_a.memory.search.stage.failed",
                        stage = "graph_augment",
                        error_code = "GRAPH_RETRIEVE_FAILED",
                        retriable = graph_retrieval_error_retriable(error),
                        elapsed_ms = stage_started.elapsed().as_millis() as u64
                    );
                })?;
            tracing::info!(
                event = "ram_a.memory.search.stage.completed",
                stage = "graph_augment",
                candidate_count = candidates.len(),
                elapsed_ms = stage_started.elapsed().as_millis() as u64
            );
            candidates
        } else {
            candidates
        };

        if self.retrieval_config.rerank.enabled {
            stage_started = Instant::now();
            tracing::info!(
                event = "ram_a.memory.search.stage.started",
                stage = "rerank",
                candidate_count = candidates.len(),
                top_k = request.top_k,
                model = self.retrieval_config.rerank.model
            );
            let results = self
                .rerank_candidates(query, candidates, request.top_k)
                .await
                .inspect_err(|_error| {
                    tracing::error!(
                        event = "ram_a.memory.search.stage.failed",
                        stage = "rerank",
                        error_code = "RERANK_FAILED",
                        retriable = true,
                        elapsed_ms = stage_started.elapsed().as_millis() as u64
                    );
                })?;
            tracing::info!(
                event = "ram_a.memory.search.stage.completed",
                stage = "rerank",
                result_count = results.len(),
                elapsed_ms = stage_started.elapsed().as_millis() as u64
            );
            Ok(results)
        } else {
            Ok(candidates)
        }
    }

    async fn rerank_candidates(
        &self,
        query: &str,
        mut candidates: Vec<ScoredMemory>,
        top_k: usize,
    ) -> MemoryResult<Vec<ScoredMemory>> {
        let Some(reranker) = self.reranker.as_ref() else {
            if self.retrieval_config.rerank.fail_open {
                tracing::warn!(
                    event = "ram_a.memory.search.degraded",
                    stage = "rerank",
                    error_code = "RERANKER_UNCONFIGURED",
                    fallback = "hybrid",
                    candidate_count = candidates.len()
                );
                candidates.truncate(top_k);
                return Ok(candidates);
            }
            return Err(MemoryError::Rerank {
                message: "rerank is enabled but no reranker is configured".to_string(),
            });
        };

        match reranker.rerank(query, candidates.clone(), top_k).await {
            Ok(mut results) => {
                results.truncate(top_k);
                Ok(results)
            }
            Err(_error) if self.retrieval_config.rerank.fail_open => {
                tracing::warn!(
                    event = "ram_a.memory.search.degraded",
                    stage = "rerank",
                    error_code = "RERANK_FAILED",
                    fallback = "hybrid",
                    candidate_count = candidates.len()
                );
                candidates.truncate(top_k);
                Ok(candidates)
            }
            Err(error) => Err(error),
        }
    }

    async fn dense_candidates(
        &self,
        query_embedding: &[f32],
        filter: Option<&serde_json::Value>,
        limit: usize,
    ) -> MemoryResult<Vec<ScoredMemory>> {
        if let Some(sqlite_store) = self.store.as_any().downcast_ref::<SqliteMemoryStore>() {
            self.validate_search_embedding_profile(filter).await?;
            return sqlite_store
                .dense_candidates(query_embedding, filter, limit)
                .await;
        }

        let mut results = self
            .store
            .list_records()
            .await?
            .into_iter()
            .filter(|record| metadata_matches(&record.metadata, filter))
            .map(|record| {
                if record.embedding.len() != query_embedding.len() {
                    return Err(MemoryError::Embedding {
                        message: format!(
                            "embedding dimension mismatch: query has {} dims but record '{}' has {} dims",
                            query_embedding.len(),
                            record.id,
                            record.embedding.len()
                        ),
                    });
                }
                let score = cosine_similarity(query_embedding, &record.embedding);
                Ok(ScoredMemory { record, score })
            })
            .collect::<Result<Vec<_>, _>>()?;

        sort_scored_desc(&mut results);
        results.truncate(limit);
        Ok(results)
    }

    async fn fuse_optional_graph_channel(
        &self,
        request: &SearchMemoryRequest,
        base_candidates: Vec<ScoredMemory>,
        limit: usize,
    ) -> MemoryResult<Vec<ScoredMemory>> {
        if !self.retrieval_config.graph.enabled {
            return Ok(base_candidates);
        }

        let graph_candidates = self.graph_candidates(request, limit).await?;
        Ok(fuse_graph_candidates(
            base_candidates,
            graph_candidates,
            limit,
            self.retrieval_config.graph.weight,
            self.retrieval_config.graph.rerank_with_graph,
            self.retrieval_config.graph.allow_graph_only,
            self.retrieval_config.graph.max_graph_only_results,
        ))
    }

    async fn graph_candidates(
        &self,
        request: &SearchMemoryRequest,
        limit: usize,
    ) -> MemoryResult<Vec<ScoredMemory>> {
        if !self.retrieval_config.graph.enabled || limit == 0 {
            return Ok(Vec::new());
        }

        let query = request.query.trim();
        if query.is_empty() {
            return Err(MemoryError::InvalidInput {
                message: "search query must not be empty".to_string(),
            });
        }

        let Some(memory_space_id) = request
            .graph_memory_space_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            if self.retrieval_config.graph.fail_open {
                return Ok(Vec::new());
            }
            return Err(MemoryError::InvalidInput {
                message: "graph retrieval is enabled but graph_memory_space_id is missing"
                    .to_string(),
            });
        };

        let Some(sqlite_store) = self.store.as_any().downcast_ref::<SqliteMemoryStore>() else {
            if self.retrieval_config.graph.fail_open {
                return Ok(Vec::new());
            }
            return Err(MemoryError::StoreBackend {
                message: "graph retrieval requires sqlite store backend".to_string(),
            });
        };

        let repository = GraphRepository::open(sqlite_store.path());
        let graph_request = GraphRetrieveContextRequest {
            memory_space_id: memory_space_id.to_string(),
            query: query.to_string(),
            top_k: limit,
            reference_time_ms: None,
            query_embedding: None,
            query_embedding_model: None,
            target_subject_entity_name: request.graph_target_subject.clone(),
            target_evidence_speaker: request.graph_target_evidence_speaker.clone(),
            seed_limit: self.retrieval_config.graph.seed_limit,
            max_evidence_records_per_fact: self
                .retrieval_config
                .graph
                .max_evidence_records_per_fact,
        };

        match repository.retrieve_context(graph_request).await {
            Ok(bundle) => {
                let mut candidates = HashMap::new();
                for unit in bundle.fact_context_units {
                    let graph_fact = graph_fact_metadata(&unit);
                    for record in unit.evidence_records {
                        if !metadata_matches(&record.metadata, request.filter.as_ref()) {
                            continue;
                        }
                        let score = unit.score;
                        let mut memory_record = graph_record_to_memory_record(record);
                        append_graph_fact_metadata(&mut memory_record.metadata, graph_fact.clone());
                        candidates
                            .entry(memory_record.id.clone())
                            .and_modify(|existing: &mut ScoredMemory| {
                                existing.score = existing.score.max(score);
                                merge_graph_facts_metadata(
                                    &mut existing.record.metadata,
                                    &memory_record.metadata,
                                );
                            })
                            .or_insert(ScoredMemory {
                                record: memory_record,
                                score,
                            });
                    }
                }
                for unit in bundle.evidence_record_context_units {
                    if !metadata_matches(&unit.record.metadata, request.filter.as_ref()) {
                        continue;
                    }
                    let score = unit.score;
                    let graph_match = graph_evidence_record_metadata(&unit);
                    let mut memory_record = graph_record_to_memory_record(unit.record);
                    append_graph_match_metadata(&mut memory_record.metadata, graph_match);
                    candidates
                        .entry(memory_record.id.clone())
                        .and_modify(|existing: &mut ScoredMemory| {
                            existing.score = existing.score.max(score);
                            merge_graph_facts_metadata(
                                &mut existing.record.metadata,
                                &memory_record.metadata,
                            );
                            merge_graph_matches_metadata(
                                &mut existing.record.metadata,
                                &memory_record.metadata,
                            );
                        })
                        .or_insert(ScoredMemory {
                            record: memory_record,
                            score,
                        });
                }
                let mut results = candidates.into_values().collect::<Vec<_>>();
                sort_scored_desc(&mut results);
                results.truncate(limit);
                Ok(results)
            }
            Err(_error) if self.retrieval_config.graph.fail_open => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    fn sqlite_store_for_mode(&self, mode: SearchMode) -> MemoryResult<&SqliteMemoryStore> {
        self.store
            .as_any()
            .downcast_ref::<SqliteMemoryStore>()
            .ok_or_else(|| MemoryError::StoreBackend {
                message: format!("{mode:?} search requires sqlite store backend"),
            })
    }

    fn embedding_profile_metadata(&self) -> serde_json::Value {
        serde_json::json!({
            "profile_id": self.embedder.profile_id(),
            "model": self.embedder.model_name(),
            "dimensions": self.embedder.dimensions(),
        })
    }

    async fn validate_new_record_embedding_profiles(
        &self,
        new_records: &[MemoryRecord],
    ) -> MemoryResult<()> {
        let Some(_sqlite_store) = self.store.as_any().downcast_ref::<SqliteMemoryStore>() else {
            return Ok(());
        };
        if new_records.is_empty() {
            return Ok(());
        }

        let mut expected_by_scope = HashMap::<String, String>::new();
        for record in self.store.list_records().await? {
            let Some(scope_id) = extract_scope_id(&record.metadata) else {
                continue;
            };
            if let Some(profile_id) = record_embedding_profile_id(&record) {
                insert_expected_embedding_profile(&mut expected_by_scope, &scope_id, &profile_id)?;
            }
        }

        let current_profile_id = self.embedder.profile_id();
        for record in new_records {
            let Some(scope_id) = extract_scope_id(&record.metadata) else {
                continue;
            };
            let profile_id =
                record_embedding_profile_id(record).unwrap_or_else(|| current_profile_id.clone());
            insert_expected_embedding_profile(&mut expected_by_scope, &scope_id, &profile_id)?;
        }

        Ok(())
    }

    async fn validate_search_embedding_profile(
        &self,
        filter: Option<&serde_json::Value>,
    ) -> MemoryResult<()> {
        let current_profile_id = self.embedder.profile_id();
        let filter_scope_id = extract_scope_id_from_filter(filter);
        for record in self.store.list_records().await? {
            if !metadata_matches(&record.metadata, filter) {
                continue;
            }
            if filter_scope_id.is_none() && extract_scope_id(&record.metadata).is_none() {
                continue;
            }
            let Some(profile_id) = record_embedding_profile_id(&record) else {
                continue;
            };
            if profile_id != current_profile_id {
                return Err(embedding_profile_mismatch_error(
                    extract_scope_id(&record.metadata).as_deref(),
                    &current_profile_id,
                    &profile_id,
                ));
            }
        }
        Ok(())
    }
}

fn metadata_with_embedding_profile(
    metadata: serde_json::Value,
    profile: &serde_json::Value,
) -> serde_json::Value {
    let mut metadata = metadata;
    if let Some(object) = metadata.as_object_mut() {
        object.insert(EMBEDDING_PROFILE_METADATA_KEY.to_string(), profile.clone());
    }
    metadata
}

fn record_embedding_profile_id(record: &MemoryRecord) -> Option<String> {
    record
        .metadata
        .get(EMBEDDING_PROFILE_METADATA_KEY)
        .and_then(|profile| profile.get("profile_id"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn insert_expected_embedding_profile(
    expected_by_scope: &mut HashMap<String, String>,
    scope_id: &str,
    profile_id: &str,
) -> MemoryResult<()> {
    if let Some(expected) = expected_by_scope.get(scope_id) {
        if expected != profile_id {
            return Err(embedding_profile_mismatch_error(
                Some(scope_id),
                expected,
                profile_id,
            ));
        }
    } else {
        expected_by_scope.insert(scope_id.to_string(), profile_id.to_string());
    }
    Ok(())
}

fn embedding_profile_mismatch_error(
    scope_id: Option<&str>,
    expected: &str,
    actual: &str,
) -> MemoryError {
    let scope = scope_id.unwrap_or("<unscoped>");
    MemoryError::Embedding {
        message: format!(
            "embedding profile mismatch for scope `{scope}`: expected `{expected}` got `{actual}`"
        ),
    }
}

fn graph_fact_metadata(unit: &FactContextUnit) -> serde_json::Value {
    serde_json::json!({
        "fact_id": unit.fact_id,
        "fact_text": unit.fact_text,
        "predicate": unit.predicate,
        "score": unit.score,
        "subject": {
            "id": unit.subject_entity.id,
            "name": unit.subject_entity.canonical_name,
            "entity_type": unit.subject_entity.entity_type,
        },
        "object": {
            "id": unit.object_entity.id,
            "name": unit.object_entity.canonical_name,
            "entity_type": unit.object_entity.entity_type,
        },
        "valid_from_ms": unit.valid_from_ms,
        "valid_to_ms": unit.valid_to_ms,
        "recorded_at_ms": unit.recorded_at_ms,
        "path": unit.path,
    })
}

fn graph_evidence_record_metadata(unit: &EvidenceRecordContextUnit) -> serde_json::Value {
    let match_kind = match unit.match_kind {
        crate::graph::EvidenceRecordMatchKind::Lexical => "lexical",
    };
    serde_json::json!({
        "kind": "evidence_record",
        "match_kind": match_kind,
        "record_id": unit.record.id,
        "score": unit.score,
        "path": unit.path,
    })
}

fn append_graph_fact_metadata(metadata: &mut serde_json::Value, graph_fact: serde_json::Value) {
    ensure_metadata_object(metadata);
    let Some(metadata_object) = metadata.as_object_mut() else {
        return;
    };
    let graph_facts = metadata_object
        .entry(GRAPH_FACTS_METADATA_KEY.to_string())
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    if !graph_facts.is_array() {
        *graph_facts = serde_json::Value::Array(Vec::new());
    }
    let Some(graph_facts) = graph_facts.as_array_mut() else {
        return;
    };
    if let Some(fact_id) = graph_fact.get("fact_id").and_then(|value| value.as_str()) {
        if graph_facts.iter().any(|existing| {
            existing.get("fact_id").and_then(|value| value.as_str()) == Some(fact_id)
        }) {
            return;
        }
    }
    let oversized = serde_json::to_vec(&graph_fact)
        .map(|encoded| encoded.len() > MAX_GRAPH_FACT_METADATA_BYTES)
        .unwrap_or(true);
    if oversized || graph_facts.len() >= MAX_GRAPH_FACTS_PER_RECORD {
        metadata_object.insert(
            GRAPH_FACTS_TRUNCATED_METADATA_KEY.to_string(),
            serde_json::Value::Bool(true),
        );
        return;
    }
    graph_facts.push(graph_fact);
}

fn merge_graph_facts_metadata(
    target_metadata: &mut serde_json::Value,
    source_metadata: &serde_json::Value,
) {
    let Some(source_graph_facts) = source_metadata
        .get(GRAPH_FACTS_METADATA_KEY)
        .and_then(|value| value.as_array())
    else {
        return;
    };
    if source_metadata
        .get(GRAPH_FACTS_TRUNCATED_METADATA_KEY)
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        ensure_metadata_object(target_metadata);
        if let Some(target) = target_metadata.as_object_mut() {
            target.insert(
                GRAPH_FACTS_TRUNCATED_METADATA_KEY.to_string(),
                serde_json::Value::Bool(true),
            );
        }
    }
    for graph_fact in source_graph_facts {
        append_graph_fact_metadata(target_metadata, graph_fact.clone());
    }
}

fn append_graph_match_metadata(metadata: &mut serde_json::Value, graph_match: serde_json::Value) {
    ensure_metadata_object(metadata);
    let Some(metadata_object) = metadata.as_object_mut() else {
        return;
    };
    let graph_matches = metadata_object
        .entry(GRAPH_MATCHES_METADATA_KEY.to_string())
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    if !graph_matches.is_array() {
        *graph_matches = serde_json::Value::Array(Vec::new());
    }
    let Some(graph_matches) = graph_matches.as_array_mut() else {
        return;
    };
    let record_id = graph_match
        .get("record_id")
        .and_then(|value| value.as_str());
    let kind = graph_match.get("kind").and_then(|value| value.as_str());
    let match_kind = graph_match
        .get("match_kind")
        .and_then(|value| value.as_str());
    if graph_matches.iter().any(|existing| {
        existing.get("record_id").and_then(|value| value.as_str()) == record_id
            && existing.get("kind").and_then(|value| value.as_str()) == kind
            && existing.get("match_kind").and_then(|value| value.as_str()) == match_kind
    }) {
        return;
    }
    graph_matches.push(graph_match);
}

fn merge_graph_matches_metadata(
    target_metadata: &mut serde_json::Value,
    source_metadata: &serde_json::Value,
) {
    let Some(source_graph_matches) = source_metadata
        .get(GRAPH_MATCHES_METADATA_KEY)
        .and_then(|value| value.as_array())
    else {
        return;
    };
    for graph_match in source_graph_matches {
        append_graph_match_metadata(target_metadata, graph_match.clone());
    }
}

fn ensure_metadata_object(metadata: &mut serde_json::Value) {
    if !metadata.is_object() {
        *metadata = serde_json::Value::Object(serde_json::Map::new());
    }
}

fn graph_record_to_memory_record(record: GraphMemoryRecord) -> MemoryRecord {
    let id = record
        .source_ref
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&record.id)
        .to_string();
    let mut metadata = record.metadata;
    if let Some(source_agent_id) = record.created_by_agent_id {
        ensure_metadata_object(&mut metadata);
        if let Some(metadata) = metadata.as_object_mut() {
            metadata.insert(
                "source_agent_id".to_string(),
                serde_json::Value::String(source_agent_id),
            );
        }
    }
    MemoryRecord {
        id,
        text: record.text,
        metadata,
        embedding: record.embedding.unwrap_or_default(),
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

struct HybridCandidate {
    record: MemoryRecord,
    dense_score: Option<f32>,
    bm25_score: Option<f32>,
}

struct GraphFusionCandidate {
    record: MemoryRecord,
    base_score: Option<f32>,
    graph_score: Option<f32>,
    base_rank: Option<usize>,
    graph_rank: Option<usize>,
}

fn fuse_hybrid_candidates(
    dense_candidates: Vec<ScoredMemory>,
    bm25_candidates: Vec<ScoredMemory>,
    top_k: usize,
    embedding_weight: f32,
    bm25_weight: f32,
) -> Vec<ScoredMemory> {
    let mut candidates = HashMap::new();
    for candidate in dense_candidates {
        candidates.insert(
            candidate.record.id.clone(),
            HybridCandidate {
                record: candidate.record,
                dense_score: Some(candidate.score),
                bm25_score: None,
            },
        );
    }

    for candidate in bm25_candidates {
        candidates
            .entry(candidate.record.id.clone())
            .and_modify(|existing| {
                existing.bm25_score = Some(candidate.score);
            })
            .or_insert_with(|| HybridCandidate {
                record: candidate.record,
                dense_score: None,
                bm25_score: Some(candidate.score),
            });
    }

    let dense_range = score_range(
        candidates
            .values()
            .filter_map(|candidate| candidate.dense_score),
    );
    let bm25_range = score_range(
        candidates
            .values()
            .filter_map(|candidate| candidate.bm25_score),
    );

    let mut results = candidates
        .into_values()
        .map(|candidate| {
            let dense_norm = normalize_present_score(candidate.dense_score, dense_range);
            let bm25_norm = normalize_present_score(candidate.bm25_score, bm25_range);
            ScoredMemory {
                record: candidate.record,
                score: embedding_weight * dense_norm + bm25_weight * bm25_norm,
            }
        })
        .collect::<Vec<_>>();

    sort_scored_desc(&mut results);
    results.truncate(top_k);
    results
}

fn fuse_graph_candidates(
    base_candidates: Vec<ScoredMemory>,
    graph_candidates: Vec<ScoredMemory>,
    top_k: usize,
    graph_weight: f32,
    rerank_with_graph: bool,
    allow_graph_only: bool,
    max_graph_only_results: Option<usize>,
) -> Vec<ScoredMemory> {
    if top_k == 0 {
        return Vec::new();
    }

    if graph_candidates.is_empty() {
        let mut results = base_candidates;
        sort_scored_desc(&mut results);
        results.truncate(top_k);
        return results;
    }

    let graph_weight = if graph_weight.is_finite() {
        graph_weight.max(0.0)
    } else {
        0.0
    };
    let mut protected_base = base_candidates;
    sort_scored_desc(&mut protected_base);
    protected_base.truncate(top_k);

    let mut base_candidates = HashMap::new();
    let mut graph_only_candidates = HashMap::new();
    for (index, candidate) in protected_base.into_iter().enumerate() {
        let id = candidate.record.id.clone();
        base_candidates
            .entry(id)
            .and_modify(|existing: &mut GraphFusionCandidate| {
                if existing
                    .base_score
                    .is_none_or(|score| candidate.score > score)
                {
                    existing.record = candidate.record.clone();
                    existing.base_score = Some(candidate.score);
                    existing.base_rank = Some(index + 1);
                }
            })
            .or_insert_with(|| GraphFusionCandidate {
                record: candidate.record,
                base_score: Some(candidate.score),
                graph_score: None,
                base_rank: Some(index + 1),
                graph_rank: None,
            });
    }

    let mut graph_candidates = graph_candidates;
    sort_scored_desc(&mut graph_candidates);
    for (index, candidate) in graph_candidates.into_iter().enumerate() {
        let id = candidate.record.id.clone();
        if let Some(existing) = base_candidates.get_mut(&id) {
            existing.graph_score = Some(
                existing
                    .graph_score
                    .map_or(candidate.score, |score| score.max(candidate.score)),
            );
            existing.graph_rank = Some(
                existing
                    .graph_rank
                    .map_or(index + 1, |rank| rank.min(index + 1)),
            );
            merge_graph_facts_metadata(&mut existing.record.metadata, &candidate.record.metadata);
            merge_graph_matches_metadata(&mut existing.record.metadata, &candidate.record.metadata);
            continue;
        }

        graph_only_candidates
            .entry(id)
            .and_modify(|existing: &mut GraphFusionCandidate| {
                if existing
                    .graph_score
                    .is_none_or(|score| candidate.score > score)
                {
                    existing.record = candidate.record.clone();
                    existing.graph_score = Some(candidate.score);
                    existing.graph_rank = Some(index + 1);
                }
            })
            .or_insert_with(|| GraphFusionCandidate {
                record: candidate.record,
                base_score: None,
                graph_score: Some(candidate.score),
                base_rank: None,
                graph_rank: Some(index + 1),
            });
    }

    let mut results = base_candidates
        .into_values()
        .map(|candidate| {
            let score = if rerank_with_graph {
                graph_rank_fusion_score(candidate.base_rank, candidate.graph_rank, graph_weight)
            } else {
                candidate.base_score.unwrap_or(0.0)
            };
            ScoredMemory {
                record: candidate.record,
                score,
            }
        })
        .collect::<Vec<_>>();

    if allow_graph_only && rerank_with_graph && graph_weight > 0.0 {
        let mut graph_only_results = graph_only_candidates
            .into_values()
            .map(|candidate| ScoredMemory {
                record: candidate.record,
                score: graph_rank_fusion_score(
                    candidate.base_rank,
                    candidate.graph_rank,
                    graph_weight,
                ),
            })
            .collect::<Vec<_>>();
        sort_scored_desc(&mut graph_only_results);
        let graph_only_limit = graph_only_result_limit(top_k, max_graph_only_results);
        graph_only_results.truncate(graph_only_limit);

        sort_scored_desc(&mut results);
        results.truncate(top_k.saturating_sub(graph_only_results.len()));
        results.extend(graph_only_results);
    }

    sort_scored_desc(&mut results);
    results.truncate(top_k);
    results
}

const GRAPH_RRF_RANK_CONSTANT: f32 = 60.0;
const DEFAULT_GRAPH_ONLY_RESULT_RATIO: f32 = 0.2;

fn graph_rank_fusion_score(
    base_rank: Option<usize>,
    graph_rank: Option<usize>,
    graph_weight: f32,
) -> f32 {
    let base_score = base_rank
        .map(|rank| 1.0 / (GRAPH_RRF_RANK_CONSTANT + rank as f32))
        .unwrap_or(0.0);
    let graph_score = graph_rank
        .map(|rank| graph_weight / (GRAPH_RRF_RANK_CONSTANT + rank as f32))
        .unwrap_or(0.0);
    base_score + graph_score
}

fn graph_only_result_limit(top_k: usize, configured_limit: Option<usize>) -> usize {
    if top_k == 0 {
        return 0;
    }
    configured_limit
        .unwrap_or_else(|| ((top_k as f32) * DEFAULT_GRAPH_ONLY_RESULT_RATIO).ceil() as usize)
        .min(top_k)
}

fn score_range(scores: impl Iterator<Item = f32>) -> Option<(f32, f32)> {
    let mut min_score = f32::INFINITY;
    let mut max_score = f32::NEG_INFINITY;
    let mut found = false;
    for score in scores {
        found = true;
        min_score = min_score.min(score);
        max_score = max_score.max(score);
    }

    found.then_some((min_score, max_score))
}

fn normalize_present_score(score: Option<f32>, range: Option<(f32, f32)>) -> f32 {
    let Some(score) = score else {
        return 0.0;
    };
    let Some((min_score, max_score)) = range else {
        return 0.0;
    };
    let range = max_score - min_score;
    if range.abs() <= f32::EPSILON {
        return 1.0;
    }
    (score - min_score) / range
}

fn sort_scored_desc(results: &mut [ScoredMemory]) {
    results.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.record.id.cmp(&right.record.id))
    });
}

fn metadata_matches_any(metadata: &serde_json::Value, filters: &[serde_json::Value]) -> bool {
    filters
        .iter()
        .any(|filter| metadata_matches(metadata, Some(filter)))
}

#[async_trait]
impl LongTermMemory for MemoryManager {
    async fn add(&self, request: AddMemoryRequest) -> MemoryResult<AddMemoryResponse> {
        let text = request.text.trim();
        if text.is_empty() {
            return Err(MemoryError::InvalidInput {
                message: "memory text must not be empty".to_string(),
            });
        }

        let embedding = self.embedder.embed_one(text).await?;
        let now = current_time_ms();
        let id = request.id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let record = MemoryRecord {
            id: id.clone(),
            text: text.to_string(),
            metadata: metadata_with_embedding_profile(
                request.metadata,
                &self.embedding_profile_metadata(),
            ),
            embedding,
            created_at_ms: now,
            updated_at_ms: now,
        };

        self.validate_new_record_embedding_profiles(std::slice::from_ref(&record))
            .await?;
        self.store.add_record(&record).await?;
        Ok(AddMemoryResponse { id })
    }

    async fn search(&self, request: SearchMemoryRequest) -> MemoryResult<Vec<ScoredMemory>> {
        match self.retrieval_config.mode {
            SearchMode::Dense => {}
            SearchMode::Bm25 => return self.search_bm25(request).await,
            SearchMode::Graph => return self.search_graph(request).await,
            SearchMode::Hybrid => return self.search_hybrid(request).await,
        }

        if request.top_k == 0 {
            return Ok(Vec::new());
        }
        let query = request.query.trim();
        if query.is_empty() {
            return Err(MemoryError::InvalidInput {
                message: "search query must not be empty".to_string(),
            });
        }

        let query_embedding = self.embedder.embed_one(query).await?;
        let candidates = self
            .dense_candidates(&query_embedding, request.filter.as_ref(), request.top_k)
            .await?;
        self.fuse_optional_graph_channel(&request, candidates, request.top_k)
            .await
    }
}

fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn graph_retrieval_error_retriable(error: &MemoryError) -> bool {
    !matches!(
        error,
        MemoryError::InvalidInput { .. } | MemoryError::StoreBackend { .. } | MemoryError::Json(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        EmbeddingProvider, FileMemoryStore, HashEmbedding, RerankConfig, Reranker, RetrievalConfig,
        SearchMode, SqliteMemoryStore,
    };
    use async_trait::async_trait;
    use std::any::Any;
    use std::sync::Mutex;

    #[test]
    fn graph_retrieval_configuration_errors_are_not_retriable() {
        assert!(!graph_retrieval_error_retriable(
            &MemoryError::InvalidInput {
                message: "missing graph memory space".to_string(),
            }
        ));
        assert!(!graph_retrieval_error_retriable(
            &MemoryError::StoreBackend {
                message: "sqlite required".to_string(),
            }
        ));
        assert!(graph_retrieval_error_retriable(&MemoryError::Embedding {
            message: "provider timed out".to_string(),
        }));
    }

    struct StaticEmbedding {
        vector: Vec<f32>,
    }

    #[async_trait]
    impl EmbeddingProvider for StaticEmbedding {
        fn dimensions(&self) -> usize {
            self.vector.len()
        }

        async fn embed(&self, texts: &[String]) -> MemoryResult<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| self.vector.clone()).collect())
        }
    }

    struct NamedEmbedding {
        vector: Vec<f32>,
        model_name: &'static str,
    }

    #[async_trait]
    impl EmbeddingProvider for NamedEmbedding {
        fn dimensions(&self) -> usize {
            self.vector.len()
        }

        fn model_name(&self) -> &str {
            self.model_name
        }

        async fn embed(&self, texts: &[String]) -> MemoryResult<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| self.vector.clone()).collect())
        }
    }

    struct FakeReranker {
        scores_by_id: HashMap<String, f32>,
        seen_candidate_counts: Arc<Mutex<Vec<usize>>>,
        fail: bool,
    }

    impl FakeReranker {
        fn with_scores(scores: &[(&str, f32)]) -> Self {
            Self {
                scores_by_id: scores
                    .iter()
                    .map(|(id, score)| ((*id).to_string(), *score))
                    .collect(),
                seen_candidate_counts: Arc::new(Mutex::new(Vec::new())),
                fail: false,
            }
        }

        fn failing() -> Self {
            Self {
                scores_by_id: HashMap::new(),
                seen_candidate_counts: Arc::new(Mutex::new(Vec::new())),
                fail: true,
            }
        }
    }

    #[async_trait]
    impl Reranker for FakeReranker {
        async fn rerank(
            &self,
            _query: &str,
            candidates: Vec<ScoredMemory>,
            top_k: usize,
        ) -> MemoryResult<Vec<ScoredMemory>> {
            self.seen_candidate_counts
                .lock()
                .expect("candidate count lock")
                .push(candidates.len());

            if self.fail {
                return Err(MemoryError::Rerank {
                    message: "fake reranker failed".to_string(),
                });
            }

            let mut results = candidates
                .into_iter()
                .map(|mut candidate| {
                    candidate.score = *self.scores_by_id.get(&candidate.record.id).unwrap_or(&0.0);
                    candidate
                })
                .collect::<Vec<_>>();
            sort_scored_desc(&mut results);
            results.truncate(top_k);
            Ok(results)
        }
    }

    #[test]
    fn graph_fact_metadata_is_bounded_and_marks_truncation() {
        let mut metadata = serde_json::json!({});
        for index in 0..17 {
            append_graph_fact_metadata(
                &mut metadata,
                serde_json::json!({"fact_id": format!("fact-{index}")}),
            );
        }

        assert_eq!(
            metadata[GRAPH_FACTS_METADATA_KEY].as_array().unwrap().len(),
            16
        );
        assert_eq!(metadata["graph_facts_truncated"], true);

        let mut oversized = serde_json::json!({});
        append_graph_fact_metadata(
            &mut oversized,
            serde_json::json!({
                "fact_id": "oversized",
                "fact_text": "x".repeat(16 * 1024 + 1)
            }),
        );
        assert!(oversized[GRAPH_FACTS_METADATA_KEY]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(oversized[GRAPH_FACTS_TRUNCATED_METADATA_KEY], true);
    }

    struct BatchOnlyStore {
        records: Mutex<Vec<MemoryRecord>>,
    }

    impl BatchOnlyStore {
        fn new() -> Self {
            Self {
                records: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl MemoryStore for BatchOnlyStore {
        fn as_any(&self) -> &dyn Any {
            self
        }

        async fn add_record(&self, _record: &MemoryRecord) -> MemoryResult<()> {
            Err(MemoryError::StoreBackend {
                message: "add_record should not be called by add_many".to_string(),
            })
        }

        async fn add_records(&self, records: &[MemoryRecord]) -> MemoryResult<()> {
            self.records
                .lock()
                .expect("records lock")
                .extend(records.iter().cloned());
            Ok(())
        }

        async fn list_records(&self) -> MemoryResult<Vec<MemoryRecord>> {
            Err(MemoryError::StoreBackend {
                message: "list_records should not be called by add_many".to_string(),
            })
        }

        async fn replace_all(&self, _records: &[MemoryRecord]) -> MemoryResult<()> {
            Err(MemoryError::StoreBackend {
                message: "replace_all should not be called by add_many".to_string(),
            })
        }
    }

    fn dense_config() -> RetrievalConfig {
        RetrievalConfig {
            mode: SearchMode::Dense,
            ..RetrievalConfig::default()
        }
    }

    fn hybrid_config_with_rerank(input_k: usize, fail_open: bool) -> RetrievalConfig {
        RetrievalConfig {
            mode: SearchMode::Hybrid,
            embedding_weight: 0.7,
            bm25_weight: 0.3,
            candidate_k: Some(3),
            graph: Default::default(),
            rerank: RerankConfig {
                enabled: true,
                input_k,
                fail_open,
                ..RerankConfig::default()
            },
        }
    }

    #[test]
    fn graph_fusion_scores_are_bounded() {
        let results = fuse_graph_candidates(
            vec![ScoredMemory {
                record: MemoryRecord {
                    id: "base".to_string(),
                    text: "base".to_string(),
                    metadata: serde_json::json!({}),
                    embedding: vec![1.0],
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
                score: 1.0,
            }],
            vec![ScoredMemory {
                record: MemoryRecord {
                    id: "graph".to_string(),
                    text: "graph".to_string(),
                    metadata: serde_json::json!({}),
                    embedding: vec![1.0],
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
                score: 1.0,
            }],
            2,
            0.2,
            true,
            true,
            None,
        );

        assert!(results.iter().all(|result| result.score <= 1.0));
    }

    #[test]
    fn graph_fusion_protects_base_candidates_by_default() {
        let results = fuse_graph_candidates(
            vec![
                ScoredMemory {
                    record: MemoryRecord {
                        id: "base-high".to_string(),
                        text: "base high".to_string(),
                        metadata: serde_json::json!({}),
                        embedding: vec![1.0],
                        created_at_ms: 1,
                        updated_at_ms: 1,
                    },
                    score: 0.9,
                },
                ScoredMemory {
                    record: MemoryRecord {
                        id: "base-low".to_string(),
                        text: "base low".to_string(),
                        metadata: serde_json::json!({}),
                        embedding: vec![1.0],
                        created_at_ms: 1,
                        updated_at_ms: 1,
                    },
                    score: 0.1,
                },
            ],
            vec![ScoredMemory {
                record: MemoryRecord {
                    id: "graph-only".to_string(),
                    text: "graph only".to_string(),
                    metadata: serde_json::json!({}),
                    embedding: vec![1.0],
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
                score: 1.0,
            }],
            2,
            0.2,
            false,
            false,
            None,
        );

        let ids = results
            .iter()
            .map(|result| result.record.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["base-high", "base-low"]);
    }

    #[test]
    fn graph_fusion_enriches_overlapping_base_candidates_without_rerank_by_default() {
        let base_candidates = vec![
            ScoredMemory {
                record: MemoryRecord {
                    id: "base-only".to_string(),
                    text: "base only".to_string(),
                    metadata: serde_json::json!({}),
                    embedding: vec![1.0],
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
                score: 0.9,
            },
            ScoredMemory {
                record: MemoryRecord {
                    id: "base-graph".to_string(),
                    text: "base graph".to_string(),
                    metadata: serde_json::json!({}),
                    embedding: vec![1.0],
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
                score: 0.1,
            },
        ];
        let graph_candidates = vec![ScoredMemory {
            record: MemoryRecord {
                id: "base-graph".to_string(),
                text: "base graph".to_string(),
                metadata: serde_json::json!({
                    GRAPH_FACTS_METADATA_KEY: [{
                        "fact_id": "fact-1",
                        "fact_text": "base graph fact"
                    }]
                }),
                embedding: vec![1.0],
                created_at_ms: 1,
                updated_at_ms: 1,
            },
            score: 1.0,
        }];
        let results = fuse_graph_candidates(
            base_candidates,
            graph_candidates,
            2,
            1.0,
            false,
            false,
            None,
        );

        let enriched = results
            .iter()
            .find(|result| result.record.id == "base-graph")
            .expect("base graph candidate");
        assert!((enriched.score - 0.1).abs() < f32::EPSILON);
        assert!(enriched
            .record
            .metadata
            .get(GRAPH_FACTS_METADATA_KEY)
            .is_some());
    }

    #[test]
    fn graph_fusion_boosts_overlapping_base_candidates_when_rerank_is_enabled() {
        let base_candidates = vec![
            ScoredMemory {
                record: MemoryRecord {
                    id: "base-only".to_string(),
                    text: "base only".to_string(),
                    metadata: serde_json::json!({}),
                    embedding: vec![1.0],
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
                score: 0.9,
            },
            ScoredMemory {
                record: MemoryRecord {
                    id: "base-graph".to_string(),
                    text: "base graph".to_string(),
                    metadata: serde_json::json!({}),
                    embedding: vec![1.0],
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
                score: 0.1,
            },
        ];
        let graph_candidates = vec![ScoredMemory {
            record: MemoryRecord {
                id: "base-graph".to_string(),
                text: "base graph".to_string(),
                metadata: serde_json::json!({}),
                embedding: vec![1.0],
                created_at_ms: 1,
                updated_at_ms: 1,
            },
            score: 1.0,
        }];
        let unboosted = fuse_graph_candidates(
            base_candidates.clone(),
            graph_candidates.clone(),
            2,
            0.0,
            true,
            false,
            None,
        );
        let boosted =
            fuse_graph_candidates(base_candidates, graph_candidates, 2, 1.0, true, false, None);

        let unboosted_score = unboosted
            .iter()
            .find(|result| result.record.id == "base-graph")
            .expect("base graph candidate")
            .score;
        let boosted_score = boosted
            .iter()
            .find(|result| result.record.id == "base-graph")
            .expect("base graph candidate")
            .score;
        assert!(boosted_score > unboosted_score);
    }

    #[test]
    fn graph_fusion_supplements_when_graph_only_is_allowed() {
        let results = fuse_graph_candidates(
            vec![ScoredMemory {
                record: MemoryRecord {
                    id: "base".to_string(),
                    text: "base".to_string(),
                    metadata: serde_json::json!({}),
                    embedding: vec![1.0],
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
                score: 0.9,
            }],
            vec![ScoredMemory {
                record: MemoryRecord {
                    id: "graph-only".to_string(),
                    text: "graph only".to_string(),
                    metadata: serde_json::json!({}),
                    embedding: vec![1.0],
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
                score: 1.0,
            }],
            2,
            1.0,
            true,
            true,
            None,
        );

        let ids = results
            .iter()
            .map(|result| result.record.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["base", "graph-only"]);
    }

    #[test]
    fn graph_fusion_uses_an_independent_graph_only_result_limit() {
        let base_candidates = (0..5)
            .map(|index| ScoredMemory {
                record: MemoryRecord {
                    id: format!("base-{index}"),
                    text: format!("base {index}"),
                    metadata: serde_json::json!({}),
                    embedding: vec![1.0],
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
                score: 1.0 - (index as f32 * 0.25),
            })
            .collect::<Vec<_>>();
        let graph_candidates = (0..5)
            .map(|index| ScoredMemory {
                record: MemoryRecord {
                    id: format!("graph-{index}"),
                    text: format!("graph {index}"),
                    metadata: serde_json::json!({}),
                    embedding: vec![1.0],
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
                score: 1.0,
            })
            .collect::<Vec<_>>();

        let results = fuse_graph_candidates(
            base_candidates,
            graph_candidates,
            5,
            0.2,
            true,
            true,
            Some(2),
        );
        let graph_only_count = results
            .iter()
            .filter(|result| result.record.id.starts_with("graph-"))
            .count();

        assert_eq!(graph_only_count, 2);
    }

    #[test]
    fn graph_rank_fusion_uses_rank_instead_of_raw_score_scale() {
        let score = graph_rank_fusion_score(Some(1), Some(2), 0.2);
        let expected = (1.0 / 61.0) + (0.2 / 62.0);

        assert!((score - expected).abs() < f32::EPSILON);
        assert!(score > graph_rank_fusion_score(Some(2), Some(3), 0.2));
    }

    #[test]
    fn graph_fusion_order_is_invariant_to_channel_score_scale() {
        fn candidate(id: &str, score: f32) -> ScoredMemory {
            ScoredMemory {
                record: MemoryRecord {
                    id: id.to_string(),
                    text: id.to_string(),
                    metadata: serde_json::json!({}),
                    embedding: vec![1.0],
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
                score,
            }
        }

        let fuse = |base_scores: [f32; 2], graph_scores: [f32; 2]| {
            fuse_graph_candidates(
                vec![
                    candidate("base-first", base_scores[0]),
                    candidate("graph-first", base_scores[1]),
                ],
                vec![
                    candidate("graph-first", graph_scores[0]),
                    candidate("base-first", graph_scores[1]),
                ],
                2,
                0.2,
                true,
                false,
                None,
            )
            .into_iter()
            .map(|result| result.record.id)
            .collect::<Vec<_>>()
        };

        assert_eq!(
            fuse([0.9, 0.1], [0.8, 0.2]),
            fuse([900.0, -20.0], [0.008, 0.002])
        );
    }

    #[test]
    fn graph_fusion_sanitizes_non_finite_graph_weight() {
        for graph_weight in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let results = fuse_graph_candidates(
                vec![ScoredMemory {
                    record: MemoryRecord {
                        id: "base".to_string(),
                        text: "base".to_string(),
                        metadata: serde_json::json!({}),
                        embedding: vec![1.0],
                        created_at_ms: 1,
                        updated_at_ms: 1,
                    },
                    score: 1.0,
                }],
                vec![ScoredMemory {
                    record: MemoryRecord {
                        id: "graph".to_string(),
                        text: "graph".to_string(),
                        metadata: serde_json::json!({}),
                        embedding: vec![1.0],
                        created_at_ms: 1,
                        updated_at_ms: 1,
                    },
                    score: 1.0,
                }],
                2,
                graph_weight,
                true,
                true,
                None,
            );

            assert!(
                results.iter().all(|result| result.score.is_finite()),
                "graph_weight {graph_weight:?} produced non-finite score: {results:?}"
            );
        }
    }

    async fn seed_rerank_hybrid_store(temp: &tempfile::TempDir) -> Arc<SqliteMemoryStore> {
        let store = Arc::new(SqliteMemoryStore::new(temp.path().join("memory.sqlite")));
        store
            .replace_all(&[
                MemoryRecord {
                    id: "hybrid-first".to_string(),
                    text: "User loves Pacific Islander melodies.".to_string(),
                    metadata: serde_json::json!({"scope_id": "scope-a"}),
                    embedding: vec![1.0, 0.0],
                    created_at_ms: 10,
                    updated_at_ms: 10,
                },
                MemoryRecord {
                    id: "rerank-winner".to_string(),
                    text: "User loves Pacific melodies remixed by the new artist.".to_string(),
                    metadata: serde_json::json!({"scope_id": "scope-a"}),
                    embedding: vec![0.8, 0.6],
                    created_at_ms: 10,
                    updated_at_ms: 10,
                },
                MemoryRecord {
                    id: "third".to_string(),
                    text: "User bought running shoes.".to_string(),
                    metadata: serde_json::json!({"scope_id": "scope-a"}),
                    embedding: vec![0.1, 0.9949874],
                    created_at_ms: 10,
                    updated_at_ms: 10,
                },
            ])
            .await
            .expect("seed sqlite records");
        store
    }

    #[tokio::test]
    async fn add_then_search_returns_relevant_memory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(FileMemoryStore::new(temp.path().join("memory.jsonl")));
        let embedder = Arc::new(HashEmbedding::new(128));
        let manager = MemoryManager::with_retrieval_config(store, embedder, dense_config());

        manager
            .add(AddMemoryRequest {
                id: Some("deploy".to_string()),
                text: "deploy requires checking health endpoint".to_string(),
                metadata: serde_json::json!({"speaker": "alice"}),
            })
            .await
            .expect("add memory");

        let results = manager
            .search(SearchMemoryRequest {
                query: "health endpoint".to_string(),
                top_k: 1,
                filter: None,
                graph_memory_space_id: None,
                graph_target_subject: None,
                graph_target_evidence_speaker: None,
            })
            .await
            .expect("search memory");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].record.id, "deploy");
    }

    #[tokio::test]
    async fn add_many_rejects_duplicate_explicit_ids() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(FileMemoryStore::new(temp.path().join("memory.jsonl")));
        let embedder = Arc::new(HashEmbedding::new(128));
        let manager = MemoryManager::new(store, embedder);

        let error = manager
            .add_many(vec![
                AddMemoryRequest {
                    id: Some("dup".to_string()),
                    text: "first memory".to_string(),
                    metadata: serde_json::json!({}),
                },
                AddMemoryRequest {
                    id: Some("dup".to_string()),
                    text: "second memory".to_string(),
                    metadata: serde_json::json!({}),
                },
            ])
            .await
            .expect_err("duplicate IDs should fail");

        assert!(format!("{error}").contains("duplicate memory id"));
    }

    #[tokio::test]
    async fn add_many_uses_store_batch_upsert_without_listing_all_records() {
        let store = Arc::new(BatchOnlyStore::new());
        let embedder = Arc::new(HashEmbedding::new(8));
        let manager = MemoryManager::new(store.clone(), embedder);

        let responses = manager
            .add_many(vec![
                AddMemoryRequest {
                    id: Some("m1".to_string()),
                    text: "first memory".to_string(),
                    metadata: serde_json::json!({}),
                },
                AddMemoryRequest {
                    id: Some("m2".to_string()),
                    text: "second memory".to_string(),
                    metadata: serde_json::json!({}),
                },
            ])
            .await
            .expect("batch add");

        assert_eq!(responses.len(), 2);
        let stored_ids = store
            .records
            .lock()
            .expect("records lock")
            .iter()
            .map(|record| record.id.clone())
            .collect::<Vec<_>>();
        assert_eq!(stored_ids, vec!["m1", "m2"]);
    }

    #[tokio::test]
    async fn sqlite_add_rejects_embedding_profile_mismatch_within_scope() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(SqliteMemoryStore::new(temp.path().join("memory.sqlite")));
        store.initialize().await.expect("initialize sqlite");
        let first = MemoryManager::with_retrieval_config(
            store.clone(),
            Arc::new(NamedEmbedding {
                vector: vec![1.0, 0.0],
                model_name: "embedding-a",
            }),
            dense_config(),
        );
        first
            .add(AddMemoryRequest {
                id: Some("m1".to_string()),
                text: "first memory".to_string(),
                metadata: serde_json::json!({"scope_id": "scope-a"}),
            })
            .await
            .expect("add first profile");

        let second = MemoryManager::with_retrieval_config(
            store,
            Arc::new(NamedEmbedding {
                vector: vec![0.0, 1.0],
                model_name: "embedding-b",
            }),
            dense_config(),
        );
        let error = second
            .add(AddMemoryRequest {
                id: Some("m2".to_string()),
                text: "second memory".to_string(),
                metadata: serde_json::json!({"scope_id": "scope-a"}),
            })
            .await
            .expect_err("same scope must reject different embedding profile");

        let message = format!("{error}");
        assert!(message.contains("embedding profile mismatch"), "{message}");
        assert!(message.contains("scope-a"), "{message}");
    }

    #[tokio::test]
    async fn sqlite_allows_different_embedding_profiles_in_different_scopes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(SqliteMemoryStore::new(temp.path().join("memory.sqlite")));
        store.initialize().await.expect("initialize sqlite");
        let first = MemoryManager::with_retrieval_config(
            store.clone(),
            Arc::new(NamedEmbedding {
                vector: vec![1.0, 0.0],
                model_name: "embedding-a",
            }),
            dense_config(),
        );
        first
            .add(AddMemoryRequest {
                id: Some("m1".to_string()),
                text: "first memory".to_string(),
                metadata: serde_json::json!({"scope_id": "scope-a"}),
            })
            .await
            .expect("add first scope");

        let second = MemoryManager::with_retrieval_config(
            store,
            Arc::new(NamedEmbedding {
                vector: vec![0.0, 1.0],
                model_name: "embedding-b",
            }),
            dense_config(),
        );
        second
            .add(AddMemoryRequest {
                id: Some("m2".to_string()),
                text: "second memory".to_string(),
                metadata: serde_json::json!({"scope_id": "scope-b"}),
            })
            .await
            .expect("different scopes may use different profiles");
    }

    #[tokio::test]
    async fn sqlite_search_rejects_embedding_profile_mismatch_within_scope() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(SqliteMemoryStore::new(temp.path().join("memory.sqlite")));
        store.initialize().await.expect("initialize sqlite");
        let writer = MemoryManager::with_retrieval_config(
            store.clone(),
            Arc::new(NamedEmbedding {
                vector: vec![1.0, 0.0],
                model_name: "embedding-a",
            }),
            dense_config(),
        );
        writer
            .add(AddMemoryRequest {
                id: Some("m1".to_string()),
                text: "first memory".to_string(),
                metadata: serde_json::json!({"scope_id": "scope-a"}),
            })
            .await
            .expect("add first profile");

        let reader = MemoryManager::with_retrieval_config(
            store,
            Arc::new(NamedEmbedding {
                vector: vec![0.0, 1.0],
                model_name: "embedding-b",
            }),
            dense_config(),
        );
        let error = reader
            .search(SearchMemoryRequest {
                query: "first".to_string(),
                top_k: 1,
                filter: Some(serde_json::json!({"scope_id": "scope-a"})),
                graph_memory_space_id: None,
                graph_target_subject: None,
                graph_target_evidence_speaker: None,
            })
            .await
            .expect_err("same scope must reject search with different embedding profile");

        let message = format!("{error}");
        assert!(message.contains("embedding profile mismatch"), "{message}");
        assert!(message.contains("scope-a"), "{message}");
    }

    #[tokio::test]
    async fn delete_by_filter_removes_only_matching_metadata_records() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(FileMemoryStore::new(temp.path().join("memory.jsonl")));
        let embedder = Arc::new(HashEmbedding::new(8));
        let manager = MemoryManager::new(store.clone(), embedder);

        manager
            .add_many(vec![
                AddMemoryRequest {
                    id: Some("doc-a-chunk".to_string()),
                    text: "alpha content".to_string(),
                    metadata: serde_json::json!({
                        "scope_id": "dataset-1",
                        "document_id": "doc-a",
                    }),
                },
                AddMemoryRequest {
                    id: Some("doc-b-chunk".to_string()),
                    text: "beta content".to_string(),
                    metadata: serde_json::json!({
                        "scope_id": "dataset-1",
                        "document_id": "doc-b",
                    }),
                },
            ])
            .await
            .expect("seed memories");

        let deleted = manager
            .delete_by_filter(serde_json::json!({
                "scope_id": "dataset-1",
                "document_id": "doc-a",
            }))
            .await
            .expect("delete filtered memories");

        assert_eq!(deleted, 1);
        let remaining = store.list_records().await.expect("list records");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "doc-b-chunk");
    }

    #[tokio::test]
    async fn delete_by_filter_removes_sqlite_search_candidates() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(SqliteMemoryStore::new(temp.path().join("memory.sqlite")));
        let embedder = Arc::new(HashEmbedding::new(8));
        let manager = MemoryManager::with_retrieval_config(
            store,
            embedder,
            RetrievalConfig {
                mode: SearchMode::Bm25,
                ..RetrievalConfig::default()
            },
        );

        manager
            .add_many(vec![
                AddMemoryRequest {
                    id: Some("doc-a-chunk".to_string()),
                    text: "alpha needle".to_string(),
                    metadata: serde_json::json!({
                        "scope_id": "dataset-1",
                        "document_id": "doc-a",
                    }),
                },
                AddMemoryRequest {
                    id: Some("doc-b-chunk".to_string()),
                    text: "beta needle".to_string(),
                    metadata: serde_json::json!({
                        "scope_id": "dataset-1",
                        "document_id": "doc-b",
                    }),
                },
            ])
            .await
            .expect("seed sqlite memories");

        manager
            .delete_by_filter(serde_json::json!({
                "scope_id": "dataset-1",
                "document_id": "doc-a",
            }))
            .await
            .expect("delete filtered sqlite memories");

        let alpha = manager
            .search(SearchMemoryRequest {
                query: "alpha".to_string(),
                top_k: 5,
                filter: Some(serde_json::json!({"scope_id": "dataset-1"})),
                graph_memory_space_id: None,
                graph_target_subject: None,
                graph_target_evidence_speaker: None,
            })
            .await
            .expect("search deleted term");
        let beta = manager
            .search(SearchMemoryRequest {
                query: "beta".to_string(),
                top_k: 5,
                filter: Some(serde_json::json!({"scope_id": "dataset-1"})),
                graph_memory_space_id: None,
                graph_target_subject: None,
                graph_target_evidence_speaker: None,
            })
            .await
            .expect("search retained term");

        assert!(alpha.is_empty());
        assert_eq!(beta.len(), 1);
        assert_eq!(beta[0].record.id, "doc-b-chunk");
    }

    #[tokio::test]
    async fn add_many_and_search_many_match_individual_operations() {
        let temp_individual = tempfile::tempdir().expect("tempdir");
        let individual_store = Arc::new(FileMemoryStore::new(
            temp_individual.path().join("memory.jsonl"),
        ));
        let individual = MemoryManager::with_retrieval_config(
            individual_store,
            Arc::new(HashEmbedding::new(128)),
            dense_config(),
        );

        let temp_batch = tempfile::tempdir().expect("tempdir");
        let batch_store = Arc::new(FileMemoryStore::new(temp_batch.path().join("memory.jsonl")));
        let batch = MemoryManager::with_retrieval_config(
            batch_store,
            Arc::new(HashEmbedding::new(128)),
            dense_config(),
        );

        let requests = vec![
            AddMemoryRequest {
                id: Some("alpha".to_string()),
                text: "alpha likes black coffee".to_string(),
                metadata: serde_json::json!({"kind": "drink"}),
            },
            AddMemoryRequest {
                id: Some("beta".to_string()),
                text: "beta prefers green tea".to_string(),
                metadata: serde_json::json!({"kind": "drink"}),
            },
            AddMemoryRequest {
                id: Some("gamma".to_string()),
                text: "gamma deployed the health endpoint".to_string(),
                metadata: serde_json::json!({"kind": "work"}),
            },
        ];

        for request in requests.clone() {
            individual.add(request).await.expect("individual add");
        }
        batch
            .add_many_with_batch_size(requests, 2)
            .await
            .expect("batch add");

        let search_requests = vec![
            SearchMemoryRequest {
                query: "green tea".to_string(),
                top_k: 2,
                filter: None,
                graph_memory_space_id: None,
                graph_target_subject: None,
                graph_target_evidence_speaker: None,
            },
            SearchMemoryRequest {
                query: "health endpoint".to_string(),
                top_k: 2,
                filter: Some(serde_json::json!({"kind": "work"})),
                graph_memory_space_id: None,
                graph_target_subject: None,
                graph_target_evidence_speaker: None,
            },
        ];

        let mut individual_ids = Vec::new();
        for request in search_requests.clone() {
            individual_ids.push(
                individual
                    .search(request)
                    .await
                    .expect("individual search")
                    .into_iter()
                    .map(|result| result.record.id)
                    .collect::<Vec<_>>(),
            );
        }
        let batch_ids = batch
            .search_many_with_batch_size(search_requests, 2)
            .await
            .expect("batch search")
            .into_iter()
            .map(|results| {
                results
                    .into_iter()
                    .map(|result| result.record.id)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        assert_eq!(batch_ids, individual_ids);
    }

    #[tokio::test]
    async fn sqlite_dense_search_matches_file_store() {
        let temp_file = tempfile::tempdir().expect("tempdir");
        let file_store = Arc::new(FileMemoryStore::new(temp_file.path().join("memory.jsonl")));
        let file_manager = MemoryManager::with_retrieval_config(
            file_store,
            Arc::new(HashEmbedding::new(128)),
            dense_config(),
        );

        let temp_sqlite = tempfile::tempdir().expect("tempdir");
        let sqlite_store = Arc::new(SqliteMemoryStore::new(
            temp_sqlite.path().join("memory.sqlite"),
        ));
        let sqlite_manager = MemoryManager::with_retrieval_config(
            sqlite_store,
            Arc::new(HashEmbedding::new(128)),
            dense_config(),
        );

        let requests = vec![
            AddMemoryRequest {
                id: Some("alpha".to_string()),
                text: "alpha likes black coffee".to_string(),
                metadata: serde_json::json!({"kind": "drink"}),
            },
            AddMemoryRequest {
                id: Some("beta".to_string()),
                text: "beta prefers green tea".to_string(),
                metadata: serde_json::json!({"kind": "drink"}),
            },
            AddMemoryRequest {
                id: Some("gamma".to_string()),
                text: "gamma deployed the health endpoint".to_string(),
                metadata: serde_json::json!({"kind": "work"}),
            },
        ];

        file_manager
            .add_many_with_batch_size(requests.clone(), 2)
            .await
            .expect("file add");
        sqlite_manager
            .add_many_with_batch_size(requests, 2)
            .await
            .expect("sqlite add");

        let search_request = SearchMemoryRequest {
            query: "green tea".to_string(),
            top_k: 2,
            filter: Some(serde_json::json!({"kind": "drink"})),
            graph_memory_space_id: None,
            graph_target_subject: None,
            graph_target_evidence_speaker: None,
        };
        let file_ids = file_manager
            .search(search_request.clone())
            .await
            .expect("file search")
            .into_iter()
            .map(|result| result.record.id)
            .collect::<Vec<_>>();
        let sqlite_ids = sqlite_manager
            .search(search_request)
            .await
            .expect("sqlite search")
            .into_iter()
            .map(|result| result.record.id)
            .collect::<Vec<_>>();

        assert_eq!(sqlite_ids, file_ids);
    }

    #[tokio::test]
    async fn sqlite_bm25_search_returns_keyword_match() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(SqliteMemoryStore::new(temp.path().join("memory.sqlite")));
        let manager = MemoryManager::with_retrieval_config(
            store,
            Arc::new(HashEmbedding::new(128)),
            RetrievalConfig {
                mode: SearchMode::Bm25,
                ..RetrievalConfig::default()
            },
        );

        manager
            .add_many_with_batch_size(
                vec![
                    AddMemoryRequest {
                        id: Some("specific".to_string()),
                        text: "User loves Pacific Islander melodies and remix albums.".to_string(),
                        metadata: serde_json::json!({"scope_id": "scope-a"}),
                    },
                    AddMemoryRequest {
                        id: Some("generic".to_string()),
                        text: "User bought new running shoes.".to_string(),
                        metadata: serde_json::json!({"scope_id": "scope-a"}),
                    },
                ],
                2,
            )
            .await
            .expect("add memories");

        let results = manager
            .search(SearchMemoryRequest {
                query: "Pacific melodies".to_string(),
                top_k: 5,
                filter: Some(serde_json::json!({"scope_id": "scope-a"})),
                graph_memory_space_id: None,
                graph_target_subject: None,
                graph_target_evidence_speaker: None,
            })
            .await
            .expect("bm25 search");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].record.id, "specific");
        assert!(results[0].score > 0.0);
    }

    #[tokio::test]
    async fn sqlite_hybrid_search_combines_dense_and_bm25_scores() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(SqliteMemoryStore::new(temp.path().join("memory.sqlite")));
        store
            .replace_all(&[
                MemoryRecord {
                    id: "dense-only".to_string(),
                    text: "User enjoys dinner recommendations.".to_string(),
                    metadata: serde_json::json!({"scope_id": "scope-a"}),
                    embedding: vec![0.9, 0.4358899],
                    created_at_ms: 10,
                    updated_at_ms: 10,
                },
                MemoryRecord {
                    id: "hybrid-winner".to_string(),
                    text: "User loves Pacific Islander melodies.".to_string(),
                    metadata: serde_json::json!({"scope_id": "scope-a"}),
                    embedding: vec![0.85, 0.5267827],
                    created_at_ms: 10,
                    updated_at_ms: 10,
                },
                MemoryRecord {
                    id: "low-dense".to_string(),
                    text: "User bought running shoes.".to_string(),
                    metadata: serde_json::json!({"scope_id": "scope-a"}),
                    embedding: vec![0.1, 0.9949874],
                    created_at_ms: 10,
                    updated_at_ms: 10,
                },
            ])
            .await
            .expect("seed sqlite records");
        let manager = MemoryManager::with_retrieval_config(
            store,
            Arc::new(StaticEmbedding {
                vector: vec![1.0, 0.0],
            }),
            RetrievalConfig {
                mode: SearchMode::Hybrid,
                embedding_weight: 0.7,
                bm25_weight: 0.3,
                candidate_k: Some(3),
                ..RetrievalConfig::default()
            },
        );

        let results = manager
            .search(SearchMemoryRequest {
                query: "Pacific melodies".to_string(),
                top_k: 2,
                filter: Some(serde_json::json!({"scope_id": "scope-a"})),
                graph_memory_space_id: None,
                graph_target_subject: None,
                graph_target_evidence_speaker: None,
            })
            .await
            .expect("hybrid search");

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].record.id, "hybrid-winner");
        assert_eq!(results[1].record.id, "dense-only");
        assert!(results[0].score > results[1].score);
    }

    #[tokio::test]
    async fn sqlite_hybrid_search_uses_zero_for_missing_score_families() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(SqliteMemoryStore::new(temp.path().join("memory.sqlite")));
        store
            .replace_all(&[
                MemoryRecord {
                    id: "dense-only".to_string(),
                    text: "User enjoys dinner recommendations.".to_string(),
                    metadata: serde_json::json!({"scope_id": "scope-a"}),
                    embedding: vec![1.0, 0.0],
                    created_at_ms: 10,
                    updated_at_ms: 10,
                },
                MemoryRecord {
                    id: "bm25-only".to_string(),
                    text: "User loves Pacific Islander melodies.".to_string(),
                    metadata: serde_json::json!({"scope_id": "scope-a"}),
                    embedding: vec![0.0, 1.0],
                    created_at_ms: 10,
                    updated_at_ms: 10,
                },
            ])
            .await
            .expect("seed sqlite records");
        let manager = MemoryManager::with_retrieval_config(
            store,
            Arc::new(StaticEmbedding {
                vector: vec![1.0, 0.0],
            }),
            RetrievalConfig {
                mode: SearchMode::Hybrid,
                embedding_weight: 0.7,
                bm25_weight: 0.3,
                candidate_k: Some(1),
                ..RetrievalConfig::default()
            },
        );

        let results = manager
            .search(SearchMemoryRequest {
                query: "Pacific melodies".to_string(),
                top_k: 2,
                filter: Some(serde_json::json!({"scope_id": "scope-a"})),
                graph_memory_space_id: None,
                graph_target_subject: None,
                graph_target_evidence_speaker: None,
            })
            .await
            .expect("hybrid search");

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].record.id, "dense-only");
        assert!((results[0].score - 0.7).abs() < 0.0001);
        assert_eq!(results[1].record.id, "bm25-only");
        assert!((results[1].score - 0.3).abs() < 0.0001);
    }

    #[tokio::test]
    async fn hybrid_search_reranks_fused_candidates_when_enabled() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = seed_rerank_hybrid_store(&temp).await;
        let reranker = Arc::new(FakeReranker::with_scores(&[
            ("rerank-winner", 0.98),
            ("hybrid-first", 0.12),
            ("third", 0.01),
        ]));
        let manager = MemoryManager::with_retrieval_config_and_reranker(
            store,
            Arc::new(StaticEmbedding {
                vector: vec![1.0, 0.0],
            }),
            hybrid_config_with_rerank(3, false),
            reranker,
        );

        let results = manager
            .search(SearchMemoryRequest {
                query: "Pacific melodies".to_string(),
                top_k: 1,
                filter: Some(serde_json::json!({"scope_id": "scope-a"})),
                graph_memory_space_id: None,
                graph_target_subject: None,
                graph_target_evidence_speaker: None,
            })
            .await
            .expect("reranked hybrid search");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].record.id, "rerank-winner");
        assert!((results[0].score - 0.98).abs() < 0.0001);
    }

    #[tokio::test]
    async fn hybrid_search_limits_candidates_before_rerank() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = seed_rerank_hybrid_store(&temp).await;
        let fake = FakeReranker::with_scores(&[
            ("rerank-winner", 0.98),
            ("hybrid-first", 0.12),
            ("third", 0.01),
        ]);
        let seen_candidate_counts = fake.seen_candidate_counts.clone();
        let manager = MemoryManager::with_retrieval_config_and_reranker(
            store,
            Arc::new(StaticEmbedding {
                vector: vec![1.0, 0.0],
            }),
            hybrid_config_with_rerank(2, false),
            Arc::new(fake),
        );

        manager
            .search(SearchMemoryRequest {
                query: "Pacific melodies".to_string(),
                top_k: 1,
                filter: Some(serde_json::json!({"scope_id": "scope-a"})),
                graph_memory_space_id: None,
                graph_target_subject: None,
                graph_target_evidence_speaker: None,
            })
            .await
            .expect("reranked hybrid search");

        assert_eq!(
            *seen_candidate_counts.lock().expect("candidate count lock"),
            vec![2]
        );
    }

    #[tokio::test]
    async fn hybrid_search_fail_closed_returns_rerank_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = seed_rerank_hybrid_store(&temp).await;
        let manager = MemoryManager::with_retrieval_config_and_reranker(
            store,
            Arc::new(StaticEmbedding {
                vector: vec![1.0, 0.0],
            }),
            hybrid_config_with_rerank(3, false),
            Arc::new(FakeReranker::failing()),
        );

        let error = manager
            .search(SearchMemoryRequest {
                query: "Pacific melodies".to_string(),
                top_k: 1,
                filter: Some(serde_json::json!({"scope_id": "scope-a"})),
                graph_memory_space_id: None,
                graph_target_subject: None,
                graph_target_evidence_speaker: None,
            })
            .await
            .expect_err("fail closed rerank should fail search");

        assert!(format!("{error}").contains("fake reranker failed"));
    }

    #[tokio::test]
    async fn hybrid_search_fail_open_returns_hybrid_order() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = seed_rerank_hybrid_store(&temp).await;
        let manager = MemoryManager::with_retrieval_config_and_reranker(
            store,
            Arc::new(StaticEmbedding {
                vector: vec![1.0, 0.0],
            }),
            hybrid_config_with_rerank(3, true),
            Arc::new(FakeReranker::failing()),
        );

        let results = manager
            .search(SearchMemoryRequest {
                query: "Pacific melodies".to_string(),
                top_k: 1,
                filter: Some(serde_json::json!({"scope_id": "scope-a"})),
                graph_memory_space_id: None,
                graph_target_subject: None,
                graph_target_evidence_speaker: None,
            })
            .await
            .expect("fail open should return hybrid results");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].record.id, "hybrid-first");
    }

    #[tokio::test]
    async fn search_fails_on_dimension_mismatch() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store_path = temp.path().join("memory.jsonl");
        let store = Arc::new(FileMemoryStore::new(store_path.clone()));

        // Add a record with 128-dim embeddings
        let embedder_128 = Arc::new(HashEmbedding::new(128));
        let manager =
            MemoryManager::with_retrieval_config(store.clone(), embedder_128, dense_config());
        manager
            .add(AddMemoryRequest {
                id: Some("m1".to_string()),
                text: "hello world".to_string(),
                metadata: serde_json::json!({}),
            })
            .await
            .expect("add");

        // Search with a different dimension embedder (64-dim)
        let embedder_64 = Arc::new(HashEmbedding::new(64));
        let manager_mismatch =
            MemoryManager::with_retrieval_config(store, embedder_64, dense_config());
        let error = manager_mismatch
            .search(SearchMemoryRequest {
                query: "hello".to_string(),
                top_k: 5,
                filter: None,
                graph_memory_space_id: None,
                graph_target_subject: None,
                graph_target_evidence_speaker: None,
            })
            .await
            .expect_err("should fail on dimension mismatch");

        let msg = format!("{error}");
        assert!(
            msg.contains("dimension mismatch"),
            "unexpected error: {msg}"
        );
        assert!(msg.contains("m1"), "error should mention record id: {msg}");
    }
}
