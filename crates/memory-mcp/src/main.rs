use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use memory_cases::config::EmbeddingProviderKind as CaseEmbeddingProviderKind;
use memory_cases::{import_documents_from_dir, CaseServiceOptions};
use memory_core::graph::{GraphTypeRegistry, LlmGraphExtractor, OpenAiCompatibleGraphLlmClient};
use memory_core::{
    sqlite::GraphRepository, EmbeddingProvider, GraphBuildPipeline, HashEmbedding, MemoryManager,
    OpenRouterEmbedding, OpenRouterReranker, RerankProvider, SqliteMemoryStore,
};
use memory_mcp::{
    create_http_router, EmbeddingProviderKind, HttpRuntime, IdempotencyRepository, MemoryService,
    ServerConfig, TokenAuthenticator,
};
use memory_pipeline::client::OpenAiCompatibleClient;
use memory_pipeline::extraction::{LlmMemoryExtractor, MemoryExtractor};
use memory_pipeline::grounding::{GroundingVerifier, LlmGroundingVerifier};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
use uuid::Uuid;

use memory_mcp::EmbeddedCaseSearchProvider;

#[derive(Parser)]
#[command(name = "ram-a-mem")]
struct Args {
    #[arg(long, value_name = "CONFIG")]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing()?;
    let startup_run_id = Uuid::new_v4().to_string();
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
    tracing::info!(
        event = "ram_a.startup.configured",
        startup_run_id = %startup_run_id,
        memory_enabled = features.memory,
        case_library_enabled = features.case_library,
        graph_memory_enabled = config.features.graph_memory.enabled,
        retrieval_mode = ?config.retrieval.mode,
        embedding_weight = config.retrieval.embedding_weight,
        bm25_weight = config.retrieval.bm25_weight,
        candidate_k = ?config.retrieval.candidate_k,
        embedding_provider = ?providers.embedding_provider,
        embedding_model = providers.embedding_model,
        embedding_dimensions = providers.embedding_dimensions,
        rerank_enabled = config.retrieval.rerank.enabled,
        rerank_provider = ?config.retrieval.rerank.provider,
        rerank_model = config.retrieval.rerank.model
    );
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
    let graph_retrieval = if features.memory && config.features.graph_memory.enabled {
        config
            .graph_memory
            .as_ref()
            .context("enabled graph_memory feature requires graph_memory configuration")?
            .retrieval
            .core_config()
    } else {
        memory_core::GraphRetrievalConfig::default()
    };
    let retrieval_config = config.retrieval.core_config(graph_retrieval);
    let manager = if retrieval_config.rerank.enabled {
        let rerank_api_key = config
            .retrieval
            .rerank
            .api_key_env
            .as_deref()
            .map(resolve_secret_env)
            .transpose()?;
        let reranker = match retrieval_config.rerank.provider {
            RerankProvider::OpenRouter => {
                Arc::new(OpenRouterReranker::from_config_with_optional_api_key(
                    rerank_api_key,
                    &retrieval_config.rerank,
                ))
            }
        };
        Arc::new(MemoryManager::with_retrieval_config_and_reranker(
            memory_store,
            embedder.clone(),
            retrieval_config,
            reranker,
        ))
    } else {
        Arc::new(MemoryManager::with_retrieval_config(
            memory_store,
            embedder.clone(),
            retrieval_config,
        ))
    };
    let mut service = MemoryService::new(manager, idempotency, extractor, verifier);
    if features.memory && config.features.graph_memory.enabled {
        let graph = config
            .graph_memory
            .as_ref()
            .context("enabled graph_memory feature requires graph_memory configuration")?;
        let graph_key = resolve_secret_env(&graph.llm_api_key_env)?;
        let registry = GraphTypeRegistry::new_default();
        let graph_client = OpenAiCompatibleGraphLlmClient::with_base_url(
            graph_key,
            graph.llm_base_url.clone(),
            graph.llm_model.clone(),
        )
        .with_timeout_ms(Some(graph.llm_timeout_ms));
        let graph_extractor = Arc::new(LlmGraphExtractor::new(
            Arc::new(graph_client),
            registry.clone(),
        ));
        let graph_pipeline = Arc::new(GraphBuildPipeline::new(
            GraphRepository::open(&database_path),
            embedder,
            graph_extractor,
            registry,
        ));
        service = service.with_graph_memory(graph_pipeline, graph.build_concurrency);
    }
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
            let ingestion_started = std::time::Instant::now();
            tracing::info!(
                event = "ram_a.case.ingestion.started",
                startup_run_id = %startup_run_id,
                dataset_id = default_dataset_id,
                source_dir = %source_dir.display(),
                provider = ?case_library.embedding_provider,
                model = case_library.embedding_model,
                dimensions = case_library.embedding_dimensions
            );
            let ingestion_span = tracing::info_span!(
                "ram_a.case.ingestion",
                startup_run_id = %startup_run_id,
                dataset_id = default_dataset_id
            );
            let imported = import_documents_from_dir(&case_service, default_dataset_id, source_dir)
                .instrument(ingestion_span)
                .await
                .inspect_err(|error| {
                    tracing::error!(event = "ram_a.case.ingestion.failed", startup_run_id = %startup_run_id, dataset_id = default_dataset_id, error_kind = memory_cases::service::observable_error_kind(error), error = memory_cases::service::observable_error_summary(error), retriable = memory_cases::service::ingestion_error_retriable(error), latency_ms = ingestion_started.elapsed().as_millis() as u64);
                })
                .context("failed to import configured case library documents")?;
            tracing::info!(event = "ram_a.case.ingestion.completed", startup_run_id = %startup_run_id, dataset_id = default_dataset_id, document_count = imported, latency_ms = ingestion_started.elapsed().as_millis() as u64);
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
    tracing::info!(event = "ram_a.startup.listening", startup_run_id = %startup_run_id, bind_address = %config.http.socket_address());
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(cancellation_token))
        .await
        .context("HTTP server failed")
}

fn init_tracing() -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().json())
        .try_init()
        .context("failed to initialize structured logging")
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
