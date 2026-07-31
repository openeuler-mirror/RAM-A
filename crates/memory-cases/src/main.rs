mod chunker;
mod config;
mod error;
mod ingestor;
mod llm;
mod model;
mod parser;
mod repo;
mod routes;
mod service;
mod token_counter;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use clap::Parser;
use memory_core::{HashEmbedding, MemoryManager, RetrievalConfig, SearchMode, SqliteMemoryStore};

use crate::config::Cli;
use crate::llm::DocumentSummaryClient;
use crate::repo::RagRepository;
use crate::service::{RagConfig, RagService};
use crate::token_counter::{Cl100kTokenCounter, TokenCounter};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let api_token = if cli.api {
        Some(cli.resolve_api_token()?)
    } else {
        None
    };
    let service = build_service(&cli)?;

    match (cli.api, cli.ingestor) {
        (true, false) => {
            routes::serve(
                cli.bind,
                service,
                api_token.expect("API mode resolves its bearer token"),
            )
            .await
        }
        (false, true) => ingestor::run(service, cli.poll_ms).await,
        _ => bail!("choose exactly one mode: --api or --ingestor"),
    }
}

fn build_service(cli: &Cli) -> Result<Arc<RagService>> {
    let storage_paths = cli.storage_paths();
    let repo = Arc::new(RagRepository::new(&storage_paths.rag_store));
    repo.initialize()?;

    let store = Arc::new(SqliteMemoryStore::new(&storage_paths.memory_store));
    let embedder = Arc::new(HashEmbedding::new(cli.embedding_dimensions));
    let retrieval = RetrievalConfig {
        mode: SearchMode::Hybrid,
        ..RetrievalConfig::default()
    };
    let memory = Arc::new(MemoryManager::with_retrieval_config(
        store, embedder, retrieval,
    ));
    let file_root = storage_paths
        .rag_store
        .parent()
        .map(|path| path.join("memory-cases-files"))
        .unwrap_or_else(|| "memory-cases-files".into());

    Ok(Arc::new(RagService::new(
        repo,
        memory,
        RagConfig {
            file_root,
            chunk_size: cli.chunk_size,
            token_counter: build_chunk_token_counter()?,
            summary_llm: build_summary_llm(cli)?,
        },
    )))
}

fn build_chunk_token_counter() -> Result<Arc<dyn TokenCounter>> {
    let counter = Cl100kTokenCounter::new()?;
    eprintln!("memory-cases chunk tokenizer: {}", counter.name());
    Ok(Arc::new(counter))
}

fn build_summary_llm(cli: &Cli) -> Result<Option<DocumentSummaryClient>> {
    let Some(model) = optional_config_value(
        cli.summary_llm_model.as_deref(),
        "MEMORY_CASES_SUMMARY_LLM_MODEL",
        Some("MEMORY_RAG_SUMMARY_LLM_MODEL"),
    ) else {
        return Ok(None);
    };
    let api_key_env = config_value(
        &cli.summary_llm_api_key_env,
        "MEMORY_CASES_SUMMARY_LLM_API_KEY_ENV",
        Some("MEMORY_RAG_SUMMARY_LLM_API_KEY_ENV"),
    );
    let api_key = match std::env::var(&api_key_env) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            eprintln!(
                "summary LLM disabled: {api_key_env} is not set; using offline document summary"
            );
            return Ok(None);
        }
    };
    let base_url = config_value(
        &cli.summary_llm_base_url,
        "MEMORY_CASES_SUMMARY_LLM_BASE_URL",
        Some("MEMORY_RAG_SUMMARY_LLM_BASE_URL"),
    );
    let timeout_ms = u64_config_value(
        cli.summary_llm_timeout_ms,
        "MEMORY_CASES_SUMMARY_LLM_TIMEOUT_MS",
        Some("MEMORY_RAG_SUMMARY_LLM_TIMEOUT_MS"),
    );

    Ok(Some(DocumentSummaryClient::new(
        api_key,
        base_url,
        model,
        Duration::from_millis(timeout_ms),
    )))
}

fn config_value(cli_value: &str, env_name: &str, legacy_env_name: Option<&str>) -> String {
    optional_config_value(Some(cli_value), env_name, legacy_env_name)
        .unwrap_or_else(|| cli_value.to_string())
}

fn optional_config_value(
    cli_value: Option<&str>,
    env_name: &str,
    legacy_env_name: Option<&str>,
) -> Option<String> {
    env_config_value(env_name)
        .or_else(|| legacy_env_name.and_then(env_config_value))
        .or_else(|| {
            cli_value
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
}

fn env_config_value(env_name: &str) -> Option<String> {
    if let Ok(value) = std::env::var(env_name) {
        let value = value.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn u64_config_value(cli_value: u64, env_name: &str, legacy_env_name: Option<&str>) -> u64 {
    u64_env_config_value(cli_value, env_name)
        .or_else(|| legacy_env_name.and_then(|name| u64_env_config_value(cli_value, name)))
        .unwrap_or(cli_value)
}

fn u64_env_config_value(cli_value: u64, env_name: &str) -> Option<u64> {
    if let Ok(value) = std::env::var(env_name) {
        if let Ok(parsed) = value.trim().parse::<u64>() {
            return Some(parsed);
        }
        eprintln!("ignoring invalid {env_name}={value:?}; using {cli_value}");
    }
    None
}
