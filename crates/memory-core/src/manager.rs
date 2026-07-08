use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use crate::record::metadata_matches;
use crate::{
    cosine_similarity, AddMemoryRequest, AddMemoryResponse, EmbeddingProvider, MemoryError,
    MemoryRecord, MemoryResult, MemoryStore, RetrievalConfig, ScoredMemory, SearchMemoryRequest,
    SearchMode, SqliteMemoryStore,
};

const DEFAULT_EMBEDDING_BATCH_SIZE: usize = 64;

#[async_trait]
pub trait LongTermMemory: Send + Sync {
    async fn add(&self, request: AddMemoryRequest) -> MemoryResult<AddMemoryResponse>;
    async fn search(&self, request: SearchMemoryRequest) -> MemoryResult<Vec<ScoredMemory>>;
}

pub struct MemoryManager {
    store: Arc<dyn MemoryStore>,
    embedder: Arc<dyn EmbeddingProvider>,
    retrieval_config: RetrievalConfig,
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
        if requests.is_empty() {
            return Ok(Vec::new());
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
        let mut responses = Vec::with_capacity(requests.len());
        let mut new_records = Vec::with_capacity(requests.len());
        for (request, embedding) in requests.into_iter().zip(embeddings) {
            let id = request.id.unwrap_or_else(|| Uuid::new_v4().to_string());
            responses.push(AddMemoryResponse { id: id.clone() });
            new_records.push(MemoryRecord {
                id,
                text: request.text.trim().to_string(),
                metadata: request.metadata,
                embedding,
                created_at_ms: now,
                updated_at_ms: now,
            });
        }

        self.store.add_records(&new_records).await?;

        Ok(responses)
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

            all_results.push(
                self.dense_candidates(&query_embedding, request.filter.as_ref(), request.top_k)
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
        sqlite_store
            .bm25_candidates(query, request.filter.as_ref(), request.top_k)
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

        Ok(fuse_hybrid_candidates(
            dense_candidates,
            bm25_candidates,
            request.top_k,
            self.retrieval_config.embedding_weight,
            self.retrieval_config.bm25_weight,
        ))
    }

    async fn dense_candidates(
        &self,
        query_embedding: &[f32],
        filter: Option<&serde_json::Value>,
        limit: usize,
    ) -> MemoryResult<Vec<ScoredMemory>> {
        if let Some(sqlite_store) = self.store.as_any().downcast_ref::<SqliteMemoryStore>() {
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

    fn sqlite_store_for_mode(&self, mode: SearchMode) -> MemoryResult<&SqliteMemoryStore> {
        self.store
            .as_any()
            .downcast_ref::<SqliteMemoryStore>()
            .ok_or_else(|| MemoryError::StoreBackend {
                message: format!("{mode:?} search requires sqlite store backend"),
            })
    }
}

struct HybridCandidate {
    record: MemoryRecord,
    dense_score: Option<f32>,
    bm25_score: Option<f32>,
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
            metadata: request.metadata,
            embedding,
            created_at_ms: now,
            updated_at_ms: now,
        };

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
        self.dense_candidates(&query_embedding, request.filter.as_ref(), request.top_k)
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
        EmbeddingProvider, FileMemoryStore, HashEmbedding, RetrievalConfig, SearchMode,
        SqliteMemoryStore,
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
            },
            SearchMemoryRequest {
                query: "health endpoint".to_string(),
                top_k: 2,
                filter: Some(serde_json::json!({"kind": "work"})),
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
            },
        );

        let results = manager
            .search(SearchMemoryRequest {
                query: "Pacific melodies".to_string(),
                top_k: 2,
                filter: Some(serde_json::json!({"scope_id": "scope-a"})),
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
            },
        );

        let results = manager
            .search(SearchMemoryRequest {
                query: "Pacific melodies".to_string(),
                top_k: 2,
                filter: Some(serde_json::json!({"scope_id": "scope-a"})),
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
