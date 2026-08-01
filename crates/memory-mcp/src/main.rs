use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
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

use memory_mcp::CaseServiceClient;

#[derive(Parser)]
#[command(name = "ram-a-mcp-server")]
struct Args {
    #[arg(long, value_name = "CONFIG")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let config = ServerConfig::load(args.config)?;
    config.validate_runtime()?;
    let features = config.features.resolve(config.case_service.is_some());
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
        let case_service = config
            .case_service
            .as_ref()
            .context("enabled case_library feature requires case_service configuration")?;
        let case_search = CaseServiceClient::from_config(case_service)
            .context("failed to construct case service client")?;
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
