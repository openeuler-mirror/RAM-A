use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use crate::record::{extract_scope_id, extract_scope_id_from_filter, metadata_matches};
use crate::{
    cosine_similarity, AddMemoryRequest, AddMemoryResponse, EmbeddingProvider,
    GraphRetrieveContextRequest, MemoryError, MemoryRecord, MemoryResult, MemoryStore, Reranker,
    RetrievalConfig, ScoredMemory, SearchMemoryRequest, SearchMode, SqliteMemoryStore,
};
use crate::{graph::GraphMemoryRecord, sqlite::GraphRepository};

const DEFAULT_EMBEDDING_BATCH_SIZE: usize = 64;
const EMBEDDING_PROFILE_METADATA_KEY: &str = "memory_core_embedding_profile";

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
        let candidates = sqlite_store
            .bm25_candidates(query, request.filter.as_ref(), request.top_k)
            .await?;
        self.fuse_optional_graph_channel(&request, candidates, request.top_k)
            .await
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
        let query_embedding = self.embedder.embed_one(query).await?;
        on_embedded(1);
        let dense_candidates = self
            .dense_candidates(&query_embedding, request.filter.as_ref(), candidate_k)
            .await?;
        let sqlite_store = self.sqlite_store_for_mode(SearchMode::Hybrid)?;
        let bm25_candidates = sqlite_store
            .bm25_candidates(query, request.filter.as_ref(), candidate_k)
            .await?;

        let result_limit = if self.retrieval_config.rerank.enabled {
            self.retrieval_config.rerank.input_limit(request.top_k)
        } else {
            request.top_k
        };
        let candidates = fuse_hybrid_candidates(
            dense_candidates,
            bm25_candidates,
            result_limit,
            self.retrieval_config.embedding_weight,
            self.retrieval_config.bm25_weight,
        );
        let candidates = self
            .fuse_optional_graph_channel(&request, candidates, result_limit)
            .await?;

        if self.retrieval_config.rerank.enabled {
            self.rerank_candidates(query, candidates, request.top_k)
                .await
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
                    for record in unit.evidence_records {
                        if !metadata_matches(&record.metadata, request.filter.as_ref()) {
                            continue;
                        }
                        let score = unit.score;
                        let memory_record = graph_record_to_memory_record(record);
                        candidates
                            .entry(memory_record.id.clone())
                            .and_modify(|existing: &mut ScoredMemory| {
                                existing.score = existing.score.max(score);
                            })
                            .or_insert(ScoredMemory {
                                record: memory_record,
                                score,
                            });
                    }
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

fn graph_record_to_memory_record(record: GraphMemoryRecord) -> MemoryRecord {
    MemoryRecord {
        id: record.id,
        text: record.text,
        metadata: record.metadata,
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
) -> Vec<ScoredMemory> {
    if graph_candidates.is_empty() {
        let mut results = base_candidates;
        sort_scored_desc(&mut results);
        results.truncate(top_k);
        return results;
    }

    let mut candidates = HashMap::new();
    for candidate in base_candidates {
        candidates.insert(
            candidate.record.id.clone(),
            GraphFusionCandidate {
                record: candidate.record,
                base_score: Some(candidate.score),
                graph_score: None,
            },
        );
    }

    for candidate in graph_candidates {
        candidates
            .entry(candidate.record.id.clone())
            .and_modify(|existing| {
                existing.graph_score = Some(candidate.score);
            })
            .or_insert_with(|| GraphFusionCandidate {
                record: candidate.record,
                base_score: None,
                graph_score: Some(candidate.score),
            });
    }

    let base_range = score_range(
        candidates
            .values()
            .filter_map(|candidate| candidate.base_score),
    );
    let graph_range = score_range(
        candidates
            .values()
            .filter_map(|candidate| candidate.graph_score),
    );
    let graph_weight = if graph_weight.is_finite() {
        graph_weight.max(0.0)
    } else {
        0.0
    };
    let total_weight = 1.0 + graph_weight;

    let mut results = candidates
        .into_values()
        .map(|candidate| {
            let base_norm = normalize_present_score(candidate.base_score, base_range);
            let graph_norm = normalize_present_score(candidate.graph_score, graph_range);
            ScoredMemory {
                record: candidate.record,
                score: (base_norm + graph_weight * graph_norm) / total_weight,
            }
        })
        .collect::<Vec<_>>();

    sort_scored_desc(&mut results);
    results.truncate(top_k);
    results
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
        );

        assert!(results.iter().all(|result| result.score <= 1.0));
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
            })
            .await
            .expect("search deleted term");
        let beta = manager
            .search(SearchMemoryRequest {
                query: "beta".to_string(),
                top_k: 5,
                filter: Some(serde_json::json!({"scope_id": "dataset-1"})),
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
            },
            SearchMemoryRequest {
                query: "health endpoint".to_string(),
                top_k: 2,
                filter: Some(serde_json::json!({"kind": "work"})),
                graph_memory_space_id: None,
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
