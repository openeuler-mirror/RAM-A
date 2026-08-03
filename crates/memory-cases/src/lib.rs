mod chunker;
pub mod config;
pub mod error;
pub mod ingestor;
mod llm;
pub mod model;
mod parser;
mod repo;
pub mod routes;
pub mod service;
mod token_counter;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use memory_core::{
    EmbeddingProvider, HashEmbedding, MemoryManager, OpenRouterEmbedding, RetrievalConfig,
    SearchMode, SqliteMemoryStore,
};

use crate::config::EmbeddingProviderKind;
use crate::llm::DocumentSummaryClient;
use crate::model::{CreateDatasetRequest, CreateDocumentFileRequest};
use crate::repo::RagRepository;
use crate::service::{RagConfig, RagService};
use crate::token_counter::{Cl100kTokenCounter, TokenCounter};

#[derive(Clone, Debug)]
pub struct CaseServiceOptions {
    pub rag_store: PathBuf,
    pub memory_store: PathBuf,
    pub embedding_provider: EmbeddingProviderKind,
    pub embedding_api_key_env: String,
    pub embedding_base_url: String,
    pub embedding_model: String,
    pub embedding_dimensions: usize,
    pub chunk_size: usize,
    pub summary_llm_model: Option<String>,
    pub summary_llm_api_key_env: String,
    pub summary_llm_base_url: String,
    pub summary_llm_timeout_ms: u64,
}

pub fn build_service(options: &CaseServiceOptions) -> Result<Arc<RagService>> {
    let repo = Arc::new(RagRepository::new(&options.rag_store));
    repo.initialize()?;

    let store = Arc::new(SqliteMemoryStore::new(&options.memory_store));
    let embedder = build_embedding_provider(options)?;
    let retrieval = RetrievalConfig {
        mode: SearchMode::Hybrid,
        ..RetrievalConfig::default()
    };
    let memory = Arc::new(MemoryManager::with_retrieval_config(
        store, embedder, retrieval,
    ));
    let file_root = options
        .rag_store
        .parent()
        .map(|path| path.join("memory-cases-files"))
        .unwrap_or_else(|| "memory-cases-files".into());

    Ok(Arc::new(RagService::new(
        repo,
        memory,
        RagConfig {
            file_root,
            chunk_size: options.chunk_size,
            token_counter: build_chunk_token_counter()?,
            summary_llm: build_summary_llm(options)?,
        },
    )))
}

pub async fn import_documents_from_dir(
    service: &RagService,
    dataset_id: &str,
    source_dir: &Path,
) -> Result<usize> {
    anyhow::ensure!(
        source_dir.is_dir(),
        "case library source_dir does not exist or is not a directory: {}",
        source_dir.display()
    );
    ensure_dataset(service, dataset_id)?;

    let existing_names = service
        .list_documents(dataset_id)?
        .documents
        .into_iter()
        .map(|document| document.name)
        .collect::<std::collections::HashSet<_>>();
    let mut imported = 0usize;
    let mut entries = std::fs::read_dir(source_dir)
        .with_context(|| {
            format!(
                "failed to read case library source_dir {}",
                source_dir.display()
            )
        })?
        .collect::<Result<Vec<_>, std::io::Error>>()?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        if !path.is_file() || !is_supported_document(&path) {
            continue;
        }
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .with_context(|| format!("case document path is not valid UTF-8: {}", path.display()))?
            .to_string();
        if existing_names.contains(&file_name) {
            continue;
        }
        let bytes = tokio::fs::read(&path)
            .await
            .with_context(|| format!("failed to read case document {}", path.display()))?;
        let response = service
            .create_document(
                dataset_id,
                CreateDocumentFileRequest {
                    id: None,
                    task_id: None,
                    name: file_name.clone(),
                    file_name: file_name.clone(),
                    mime_type: mime_type_for_file(&path).map(str::to_string),
                    bytes,
                },
            )
            .await
            .with_context(|| format!("failed to create case document {}", path.display()))?;
        while service
            .run_next_ingestion_task()
            .await
            .with_context(|| format!("failed to ingest case document {}", response.document_id))?
        {
        }
        imported = imported.saturating_add(1);
    }

    Ok(imported)
}

fn ensure_dataset(service: &RagService, dataset_id: &str) -> Result<()> {
    if service
        .list_datasets()?
        .datasets
        .iter()
        .any(|dataset| dataset.id == dataset_id)
    {
        return Ok(());
    }
    service.create_dataset(CreateDatasetRequest {
        id: Some(dataset_id.to_string()),
        name: dataset_id.to_string(),
        description: Some("RAM-A configured case library".to_string()),
    })?;
    Ok(())
}

fn build_embedding_provider(options: &CaseServiceOptions) -> Result<Arc<dyn EmbeddingProvider>> {
    match options.embedding_provider {
        EmbeddingProviderKind::Hash => {
            Ok(Arc::new(HashEmbedding::new(options.embedding_dimensions)))
        }
        EmbeddingProviderKind::OpenAiCompatible => {
            let api_key = std::env::var(&options.embedding_api_key_env).map_err(|_| {
                anyhow::anyhow!(
                    "embedding API key environment variable `{}` is unavailable",
                    options.embedding_api_key_env
                )
            })?;
            anyhow::ensure!(
                !api_key.trim().is_empty(),
                "embedding API key environment variable `{}` is empty",
                options.embedding_api_key_env
            );
            anyhow::ensure!(
                !options.embedding_base_url.trim().is_empty(),
                "embedding base URL is empty"
            );
            anyhow::ensure!(
                !options.embedding_model.trim().is_empty(),
                "embedding model is empty"
            );
            Ok(Arc::new(OpenRouterEmbedding::with_base_url(
                api_key,
                &options.embedding_base_url,
                &options.embedding_model,
                options.embedding_dimensions,
            )))
        }
    }
}

fn build_chunk_token_counter() -> Result<Arc<dyn TokenCounter>> {
    let counter = Cl100kTokenCounter::new()?;
    eprintln!("memory-cases chunk tokenizer: {}", counter.name());
    Ok(Arc::new(counter))
}

fn build_summary_llm(options: &CaseServiceOptions) -> Result<Option<DocumentSummaryClient>> {
    let Some(model) = options.summary_llm_model.as_deref() else {
        return Ok(None);
    };
    let api_key = match std::env::var(&options.summary_llm_api_key_env) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            eprintln!(
                "summary LLM disabled: {} is not set; using offline document summary",
                options.summary_llm_api_key_env
            );
            return Ok(None);
        }
    };

    Ok(Some(DocumentSummaryClient::new(
        api_key,
        &options.summary_llm_base_url,
        model,
        Duration::from_millis(options.summary_llm_timeout_ms),
    )))
}

fn is_supported_document(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "md" | "markdown" | "mdx" | "txt" | "text" | "log"
            )
        })
        .unwrap_or(false)
}

fn mime_type_for_file(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "md" | "markdown" | "mdx" => Some("text/markdown"),
        "txt" | "text" | "log" => Some("text/plain"),
        _ => None,
    }
}
