use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use memory_cases::{
    import_documents_from_dir, CaseServiceOptions,
    EmbeddingProviderKind as CaseEmbeddingProviderKind,
};
use memory_core::graph::{GraphTypeRegistry, LlmGraphExtractor, OpenAiCompatibleGraphLlmClient};
use memory_core::{
    sqlite::GraphRepository, EmbeddingProvider, GraphBuildPipeline, HashEmbedding, MemoryManager,
    OpenRouterEmbedding, RetrievalConfig, SqliteMemoryStore,
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
    let mut retrieval_config = RetrievalConfig::default();
    if features.memory && config.features.graph_memory.enabled {
        retrieval_config.graph = config
            .graph_memory
            .as_ref()
            .context("enabled graph_memory feature requires graph_memory configuration")?
            .retrieval
            .core_config();
    }
    let manager = Arc::new(MemoryManager::with_retrieval_config(
        memory_store,
        embedder.clone(),
        retrieval_config,
    ));
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
    let listener = tokio::net::TcpListener::bind(config.http.socket_address())
        .await
        .context("failed to bind HTTP listener")?;
    let mut case_api_router = None;
    let mut ingestion_worker = None;
    let mut case_import_worker = None;
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
        let recovered = case_service
            .recover_interrupted_ingestion_tasks()
            .context("failed to recover interrupted case ingestion tasks")?;
        if recovered > 0 {
            eprintln!("case ingestion recovered {recovered} interrupted tasks");
        }
        if let Some(source_dir) = case_library.source_dir.as_deref() {
            let default_dataset_id = case_library
                .libraries
                .iter()
                .find(|library| library.name == case_library.default_library)
                .map(|library| library.dataset_id.clone())
                .context("default case library mapping is unavailable")?;
            let source_dir = source_dir.to_owned();
            let import_service = case_service.clone();
            case_import_worker = Some(tokio::spawn(async move {
                match import_documents_from_dir(&import_service, &default_dataset_id, &source_dir)
                    .await
                {
                    Ok(imported) => eprintln!(
                        "case library queued {imported} new documents from {}",
                        source_dir.display()
                    ),
                    Err(error) => eprintln!(
                        "case library source import failed for {}: {error:#}",
                        source_dir.display()
                    ),
                }
            }));
        }
        if let Some(api_token_env) = case_library.api_token_env.as_deref() {
            let api_token =
                resolve_canonical_secret_env(api_token_env, "case library API credential")?;
            case_api_router = Some(memory_cases::routes::create_api_router(
                case_service.clone(),
                api_token,
            ));
        }
        ingestion_worker = Some(tokio::spawn(memory_cases::ingestor::run_until_cancelled(
            case_service.clone(),
            case_library.ingestion_poll_ms,
            cancellation_token.clone(),
        )));
        let case_search = EmbeddedCaseSearchProvider::new(
            case_service,
            case_library.default_library.clone(),
            &case_library.libraries,
        );
        runtime.with_case_search_provider(Arc::new(case_search))
    } else {
        runtime
    };
    let mut app = create_http_router(runtime, &config.http, &config.limits);
    if let Some(case_api_router) = case_api_router {
        app = app.merge(case_api_router);
    }
    let server_result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(cancellation_token.clone()))
        .await;
    cancellation_token.cancel();
    if let Some(case_import_worker) = case_import_worker {
        case_import_worker
            .await
            .context("case library import worker failed to join")?;
    }
    if let Some(ingestion_worker) = ingestion_worker {
        ingestion_worker
            .await
            .context("case ingestion worker failed to join")?;
    }
    server_result.context("HTTP server failed")
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

fn resolve_canonical_secret_env(name: &str, label: &str) -> Result<String> {
    let value = resolve_secret_env(name)?;
    if value.trim().is_empty() || value.trim() != value {
        anyhow::bail!("{label} environment variable `{name}` must be canonical and non-empty");
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
