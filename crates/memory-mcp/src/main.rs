use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use memory_cases::config::EmbeddingProviderKind as CaseEmbeddingProviderKind;
use memory_cases::{import_documents_from_dir, CaseServiceOptions};
use memory_core::{
    EmbeddingProvider, HashEmbedding, MemoryManager, OpenRouterEmbedding, SqliteMemoryStore,
};
use memory_mcp::{
    create_http_router, EmbeddingProviderKind, HttpRuntime, IdempotencyRepository, MemoryService,
    ServerConfig, TokenAuthenticator,
};
use memory_pipeline::client::OpenAiCompatibleClient;
use memory_pipeline::extraction::{LlmMemoryExtractor, MemoryExtractor};
use memory_pipeline::grounding::{GroundingVerifier, LlmGroundingVerifier};
use tokio_util::sync::CancellationToken;

use memory_mcp::EmbeddedCaseSearchProvider;

#[derive(Parser)]
#[command(name = "ram-a-mem")]
struct Args {
    #[arg(long, value_name = "CONFIG")]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let config_path = resolve_config_path(args.config)?;
    let config = ServerConfig::load(&config_path)?;
    config.validate_runtime()?;
    let features = config.features.resolve(config.case_library.is_some());
    let authenticator = Arc::new(TokenAuthenticator::from_config(&config.auth)?);
    let storage = config
        .storage
        .as_ref()
        .context("validated storage configuration is unavailable")?;
    let database_path = storage.database_path.clone();
    let memory_store = Arc::new(SqliteMemoryStore::new(&database_path));
    memory_store
        .initialize()
        .await
        .map_err(|_| anyhow::anyhow!("failed to initialize SQLite memory storage"))?;
    let idempotency = IdempotencyRepository::open(&database_path)
        .await
        .map_err(|_| anyhow::anyhow!("failed to initialize SQLite idempotency storage"))?;
    let providers = config
        .providers
        .as_ref()
        .context("validated provider configuration is unavailable")?;
    let provider_key = resolve_secret_env(&providers.api_key_env)?;
    let embedder: Arc<dyn EmbeddingProvider> = match providers.embedding_provider {
        EmbeddingProviderKind::OpenAiCompatible => {
            let embedding_key_env = providers
                .embedding_api_key_env
                .as_deref()
                .unwrap_or(&providers.api_key_env);
            let embedding_key = resolve_secret_env(embedding_key_env)?;
            let embedding_base_url = providers
                .embedding_base_url
                .as_deref()
                .unwrap_or(&providers.base_url);
            Arc::new(OpenRouterEmbedding::with_base_url(
                embedding_key,
                embedding_base_url,
                &providers.embedding_model,
                providers.embedding_dimensions,
            ))
        }
        EmbeddingProviderKind::Hash => Arc::new(HashEmbedding::new(providers.embedding_dimensions)),
    };
    let model_client = OpenAiCompatibleClient::new(
        provider_key,
        &providers.base_url,
        providers.timeout_seconds,
        providers.max_retries,
    )
    .context("failed to construct model client")?;
    let extractor: Arc<dyn MemoryExtractor> = Arc::new(LlmMemoryExtractor::new(
        model_client.clone(),
        &providers.extractor_model,
    ));
    let verifier: Arc<dyn GroundingVerifier> = Arc::new(LlmGroundingVerifier::new(
        model_client,
        &providers.verifier_model,
    ));
    let manager = Arc::new(MemoryManager::new(memory_store, embedder));
    let service = MemoryService::new(manager, idempotency, extractor, verifier);
    let cancellation_token = CancellationToken::new();
    let runtime = HttpRuntime::with_cancellation_token(
        service,
        authenticator,
        database_path,
        true,
        cancellation_token.clone(),
    )
    .with_features(features);
    let runtime = if features.case_library {
        let case_library = config
            .case_library
            .as_ref()
            .context("enabled case_library feature requires case_library configuration")?;
        let case_options = CaseServiceOptions {
            rag_store: case_library.rag_store.clone(),
            memory_store: case_library.index_store.clone(),
            embedding_provider: match case_library.embedding_provider {
                EmbeddingProviderKind::Hash => CaseEmbeddingProviderKind::Hash,
                EmbeddingProviderKind::OpenAiCompatible => {
                    CaseEmbeddingProviderKind::OpenAiCompatible
                }
            },
            embedding_api_key_env: case_library
                .embedding_api_key_env
                .clone()
                .unwrap_or_else(|| providers.api_key_env.clone()),
            embedding_base_url: case_library
                .embedding_base_url
                .clone()
                .unwrap_or_else(|| providers.base_url.clone()),
            embedding_model: case_library.embedding_model.clone(),
            embedding_dimensions: case_library.embedding_dimensions,
            chunk_size: case_library.chunk_size,
            summary_llm_model: case_library.summary_llm_model.clone(),
            summary_llm_api_key_env: case_library
                .summary_llm_api_key_env
                .clone()
                .unwrap_or_else(|| providers.api_key_env.clone()),
            summary_llm_base_url: case_library
                .summary_llm_base_url
                .clone()
                .unwrap_or_else(|| providers.base_url.clone()),
            summary_llm_timeout_ms: case_library.summary_llm_timeout_ms,
        };
        let case_service = memory_cases::build_service(&case_options)
            .context("failed to construct embedded case library")?;
        if let Some(source_dir) = case_library.source_dir.as_deref() {
            let default_dataset_id = case_library
                .libraries
                .iter()
                .find(|library| library.name == case_library.default_library)
                .map(|library| library.dataset_id.as_str())
                .context("default case library mapping is unavailable")?;
            let imported = import_documents_from_dir(&case_service, default_dataset_id, source_dir)
                .await
                .context("failed to import configured case library documents")?;
            eprintln!(
                "case library imported {imported} new documents from {}",
                source_dir.display()
            );
        }
        let case_search = EmbeddedCaseSearchProvider::new(
            case_service,
            case_library.default_library.clone(),
            &case_library.libraries,
        );
        runtime.with_case_search_provider(Arc::new(case_search))
    } else {
        runtime
    };
    let app = create_http_router(runtime, &config.http, &config.limits);
    let listener = tokio::net::TcpListener::bind(config.http.socket_address())
        .await
        .context("failed to bind HTTP listener")?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(cancellation_token))
        .await
        .context("HTTP server failed")
}

fn resolve_config_path(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    if let Some(path) = std::env::var_os("RAM_A_MEM_CONFIG") {
        return Ok(PathBuf::from(path));
    }

    let mut candidates = vec![PathBuf::from("config/ram-a-mem.json")];
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".config/ram-a/ram-a-mem.json"));
    }
    candidates.push(PathBuf::from("/etc/ram-a/ram-a-mem.json"));

    candidates
        .into_iter()
        .find(|path| path.is_file())
        .context(
            "RAM-A memory config not found; pass --config, set RAM_A_MEM_CONFIG, or create config/ram-a-mem.json",
        )
}

fn resolve_secret_env(name: &str) -> Result<String> {
    let value = std::env::var_os(name).with_context(|| {
        format!("provider credential environment variable `{name}` is unavailable")
    })?;
    let value = value.into_string().map_err(|_| {
        anyhow::anyhow!("provider credential environment variable `{name}` is not valid Unicode")
    })?;
    if value.is_empty() {
        anyhow::bail!("provider credential environment variable `{name}` is empty");
    }
    Ok(value)
}

async fn shutdown_signal(cancellation_token: CancellationToken) {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    cancellation_token.cancel();
}
