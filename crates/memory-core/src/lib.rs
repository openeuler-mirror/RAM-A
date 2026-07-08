pub mod api;
pub mod embedding;
pub mod error;
pub mod manager;
pub mod record;
pub mod rerank;
pub mod sqlite_store;
pub mod store;
pub mod vector;

pub use api::{
    AddMemoryRequest, AddMemoryResponse, RerankConfig, RerankProvider, RetrievalConfig,
    ScoredMemory, SearchMemoryRequest, SearchMode,
};
pub use embedding::{EmbeddingProvider, HashEmbedding, OpenRouterEmbedding};
pub use error::{MemoryError, MemoryResult};
pub use manager::{LongTermMemory, MemoryManager};
pub use record::MemoryRecord;
pub use rerank::{OpenRouterReranker, Reranker};
pub use sqlite_store::SqliteMemoryStore;
pub use store::{FileMemoryStore, MemoryStore};
pub use vector::cosine_similarity;
