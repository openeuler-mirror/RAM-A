use std::collections::{HashMap, HashSet};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use memory_core::graph::{
    GraphBuildPipeline, GraphTypeRegistry, LlmGraphExtractor, OpenAiCompatibleGraphLlmClient,
};
use memory_core::{
    AddMemoryRequest, EmbeddingProvider, FileMemoryStore, GraphAddMemoryRequest,
    GraphRetrievalConfig, HashEmbedding, LongTermMemory, MemoryManager, MemoryStore,
    OpenRouterEmbedding, OpenRouterReranker, RerankConfig, RerankProvider, Reranker,
    RetrievalConfig, SearchMemoryRequest, SearchMode, SqliteMemoryStore,
};
use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;

const PREPARED_SCHEMA_VERSION: &str = "benchmark-prepared-v1";

#[derive(Parser)]
#[command(name = "memory-bench")]
#[command(about = "Add/search runner for long-term memory benchmark baselines")]
struct Cli {
    #[arg(long, default_value = "data/memory.sqlite")]
    store: PathBuf,
    #[arg(long, value_enum, default_value_t = StoreBackendKind::Sqlite)]
    store_backend: StoreBackendKind,
    #[arg(long, value_enum, default_value_t = EmbeddingKind::Openrouter)]
    embedding: EmbeddingKind,
    #[arg(long, value_enum, default_value_t = SearchModeKind::Hybrid)]
    search_mode: SearchModeKind,
    #[arg(long, default_value_t = 0.7)]
    embedding_weight: f32,
    #[arg(long, default_value_t = 0.3)]
    bm25_weight: f32,
    #[arg(long)]
    candidate_k: Option<usize>,
    #[arg(long)]
    rerank: bool,
    #[arg(long, value_enum, default_value_t = RerankProviderKind::Openrouter)]
    rerank_provider: RerankProviderKind,
    #[arg(long, default_value = "cohere/rerank-v3.5")]
    rerank_model: String,
    #[arg(long, default_value = "OPENROUTER_API_KEY")]
    rerank_api_key_env: String,
    #[arg(long, default_value = "https://openrouter.ai/api/v1")]
    rerank_base_url: String,
    #[arg(long, default_value_t = 40)]
    rerank_input_k: usize,
    #[arg(long)]
    rerank_timeout_ms: Option<u64>,
    #[arg(long)]
    rerank_fail_open: bool,
    #[arg(long)]
    graph_build: bool,
    #[arg(long)]
    graph: bool,
    #[arg(long, default_value_t = 0.2)]
    graph_weight: f32,
    #[arg(long)]
    graph_rerank: bool,
    #[arg(long)]
    graph_allow_graph_only: bool,
    #[arg(long)]
    graph_fail_open: bool,
    #[arg(long, value_enum, default_value_t = GraphMemorySpaceMode::Auto)]
    graph_memory_space_mode: GraphMemorySpaceMode,
    #[arg(long, default_value = "scope_id")]
    graph_memory_space_field: String,
    #[arg(long, default_value = "benchmark")]
    graph_owner_id: String,
    #[arg(long, default_value = "OPENROUTER_API_KEY")]
    graph_llm_api_key_env: String,
    #[arg(long, default_value = "openai/gpt-4o-mini")]
    graph_llm_model: String,
    #[arg(long, default_value = "https://openrouter.ai/api/v1")]
    graph_llm_base_url: String,
    #[arg(long)]
    graph_llm_timeout_ms: Option<u64>,
    #[arg(long, default_value_t = 1, value_parser = parse_graph_build_concurrency)]
    graph_build_concurrency: usize,
    #[arg(long, default_value = "OPENROUTER_API_KEY")]
    api_key_env: String,
    #[arg(long, default_value = "baai/bge-m3")]
    model: String,
    #[arg(long, default_value_t = 1024)]
    dimensions: usize,
    #[arg(long, default_value_t = 64)]
    batch_size: usize,
    #[command(subcommand)]
    command: Command,
}

fn parse_graph_build_concurrency(value: &str) -> std::result::Result<usize, String> {
    let concurrency = value
        .parse::<usize>()
        .map_err(|_| "must be a positive integer".to_string())?;
    if concurrency == 0 {
        return Err("must be at least 1".to_string());
    }
    Ok(concurrency)
}

#[derive(Clone, Debug, ValueEnum)]
enum EmbeddingKind {
    Openrouter,
    Hash,
}

#[derive(Clone, Debug, ValueEnum)]
enum StoreBackendKind {
    Jsonl,
    Sqlite,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SearchModeKind {
    Dense,
    Bm25,
    Graph,
    Hybrid,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum RerankProviderKind {
    Openrouter,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum GraphMemorySpaceMode {
    Auto,
    MetadataField,
    PathPrefix,
}

impl From<SearchModeKind> for SearchMode {
    fn from(value: SearchModeKind) -> Self {
        match value {
            SearchModeKind::Dense => SearchMode::Dense,
            SearchModeKind::Bm25 => SearchMode::Bm25,
            SearchModeKind::Graph => SearchMode::Graph,
            SearchModeKind::Hybrid => SearchMode::Hybrid,
        }
    }
}

impl From<RerankProviderKind> for RerankProvider {
    fn from(value: RerankProviderKind) -> Self {
        match value {
            RerankProviderKind::Openrouter => RerankProvider::OpenRouter,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    Add {
        #[arg(long)]
        dataset: PathBuf,
        #[arg(
            long,
            value_delimiter = ',',
            default_value = "text,content,message,memory"
        )]
        text_fields: Vec<String>,
        #[arg(long)]
        resume: bool,
    },
    Search {
        #[arg(long)]
        dataset: Option<PathBuf>,
        #[arg(long)]
        query: Option<String>,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value_t = 10)]
        top_k: usize,
        #[arg(long, value_delimiter = ',', default_value = "question,query")]
        query_fields: Vec<String>,
        #[arg(long)]
        filter: Option<String>,
        #[arg(long)]
        resume: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let runtime = build_runtime(&cli)?;

    match &cli.command {
        Command::Add {
            dataset,
            text_fields,
            resume,
        } => {
            let options = AddRunOptions {
                dataset: dataset.clone(),
                text_fields: text_fields.as_slice(),
                batch_size: cli.batch_size,
                resume: *resume,
                cli: &cli,
            };
            run_add(
                runtime.manager,
                runtime.store,
                runtime.graph_pipeline,
                options,
            )
            .await
        }
        Command::Search {
            dataset,
            query,
            output,
            top_k,
            query_fields,
            filter,
            resume,
        } => {
            let options = SearchRunOptions {
                output: output.clone(),
                top_k: *top_k,
                query_fields: query_fields.as_slice(),
                filter: filter.clone(),
                batch_size: cli.batch_size,
                resume: *resume,
            };
            run_search(
                runtime.manager,
                dataset.clone(),
                query.clone(),
                options,
                &cli,
            )
            .await
        }
    }
}

struct BenchRuntime {
    manager: MemoryManager,
    store: Arc<dyn MemoryStore>,
    graph_pipeline: Option<Arc<GraphBuildPipeline>>,
}

struct AddRunOptions<'a> {
    dataset: PathBuf,
    text_fields: &'a [String],
    batch_size: usize,
    resume: bool,
    cli: &'a Cli,
}

fn build_runtime(cli: &Cli) -> Result<BenchRuntime> {
    if matches!(cli.search_mode, SearchModeKind::Graph) && !cli.graph {
        bail!("--search-mode graph requires --graph");
    }
    let store = build_store(cli);
    let embedder: Arc<dyn EmbeddingProvider> = match cli.embedding {
        EmbeddingKind::Openrouter => {
            let api_key = std::env::var(&cli.api_key_env)
                .with_context(|| format!("missing API key env {}", cli.api_key_env))?;
            Arc::new(OpenRouterEmbedding::new(
                api_key,
                cli.model.clone(),
                cli.dimensions,
            ))
        }
        EmbeddingKind::Hash => Arc::new(HashEmbedding::new(cli.dimensions)),
    };
    let rerank_config = RerankConfig {
        enabled: cli.rerank,
        provider: cli.rerank_provider.into(),
        model: cli.rerank_model.clone(),
        api_key_env: cli.rerank_api_key_env.clone(),
        base_url: cli.rerank_base_url.clone(),
        input_k: cli.rerank_input_k,
        timeout_ms: cli.rerank_timeout_ms,
        fail_open: cli.rerank_fail_open,
    };
    let reranker = build_reranker(cli, &rerank_config)?;
    let graph_pipeline = build_graph_pipeline(cli, &store, embedder.clone())?;
    let retrieval_config = RetrievalConfig {
        mode: cli.search_mode.into(),
        embedding_weight: cli.embedding_weight,
        bm25_weight: cli.bm25_weight,
        candidate_k: cli.candidate_k,
        graph: GraphRetrievalConfig {
            enabled: cli.graph,
            weight: cli.graph_weight,
            rerank_with_graph: cli.graph_rerank,
            allow_graph_only: cli.graph_allow_graph_only,
            seed_limit: None,
            max_evidence_records_per_fact: None,
            fail_open: cli.graph_fail_open,
        },
        rerank: rerank_config,
    };
    let manager = if let Some(reranker) = reranker {
        MemoryManager::with_retrieval_config_and_reranker(
            store.clone(),
            embedder,
            retrieval_config,
            reranker,
        )
    } else {
        MemoryManager::with_retrieval_config(store.clone(), embedder, retrieval_config)
    };
    Ok(BenchRuntime {
        manager,
        store,
        graph_pipeline,
    })
}

fn build_graph_pipeline(
    cli: &Cli,
    store: &Arc<dyn MemoryStore>,
    embedder: Arc<dyn EmbeddingProvider>,
) -> Result<Option<Arc<GraphBuildPipeline>>> {
    if !cli.graph_build {
        return Ok(None);
    }
    let Some(sqlite_store) = store.as_any().downcast_ref::<SqliteMemoryStore>() else {
        bail!("--graph-build requires --store-backend sqlite");
    };
    let api_key = std::env::var(&cli.graph_llm_api_key_env).with_context(|| {
        format!(
            "missing graph LLM API key env {}",
            cli.graph_llm_api_key_env
        )
    })?;
    let repository = memory_core::sqlite::GraphRepository::open(sqlite_store.path());
    let registry = GraphTypeRegistry::new_default();
    let client = OpenAiCompatibleGraphLlmClient::with_base_url(
        api_key,
        cli.graph_llm_base_url.clone(),
        cli.graph_llm_model.clone(),
    )
    .with_timeout_ms(cli.graph_llm_timeout_ms);
    let extractor = Arc::new(LlmGraphExtractor::new(Arc::new(client), registry.clone()));
    Ok(Some(Arc::new(GraphBuildPipeline::new(
        repository, embedder, extractor, registry,
    ))))
}

fn build_reranker(cli: &Cli, rerank_config: &RerankConfig) -> Result<Option<Arc<dyn Reranker>>> {
    if !rerank_config.enabled {
        return Ok(None);
    }

    match cli.rerank_provider {
        RerankProviderKind::Openrouter => {
            let api_key = std::env::var(&rerank_config.api_key_env).with_context(|| {
                format!("missing rerank API key env {}", rerank_config.api_key_env)
            })?;
            Ok(Some(Arc::new(OpenRouterReranker::from_config(
                api_key,
                rerank_config,
            ))))
        }
    }
}

fn build_store(cli: &Cli) -> Arc<dyn MemoryStore> {
    match cli.store_backend {
        StoreBackendKind::Jsonl => Arc::new(FileMemoryStore::new(&cli.store)),
        StoreBackendKind::Sqlite => Arc::new(SqliteMemoryStore::new(&cli.store)),
    }
}

async fn run_add(
    manager: MemoryManager,
    store: Arc<dyn MemoryStore>,
    graph_pipeline: Option<Arc<GraphBuildPipeline>>,
    options: AddRunOptions<'_>,
) -> Result<()> {
    let json = load_json(&options.dataset)?;
    if is_prepared_schema_v1(&json) && json.get("memories").is_some() {
        return run_add_prepared_memories(manager, store, graph_pipeline, &json, options).await;
    }

    let mut texts = Vec::new();
    collect_field_texts(&json, "$", options.text_fields, &mut texts);

    if texts.is_empty() {
        bail!(
            "no memory texts found in {} using fields {:?}",
            options.dataset.display(),
            options.text_fields
        );
    }

    let existing_ids = existing_ids_for_resume(&store, options.resume).await?;
    let mut summary = AddSummary {
        total: texts.len(),
        ..Default::default()
    };
    let mut requests = Vec::with_capacity(texts.len());
    let mut graph_requests = Vec::new();
    for (index, item) in texts.iter().enumerate() {
        let id = format!("{}:{}", item.path, index);
        let mut metadata = serde_json::json!({
            "dataset": options.dataset.display().to_string(),
            "path": item.path,
            "field": item.field,
        });
        merge_metadata(&mut metadata, &item.metadata);

        if graph_pipeline.is_some() {
            graph_requests.push(build_graph_add_request(
                options.cli,
                &id,
                &item.text,
                metadata.clone(),
                &item.path,
                false,
            )?);
        }

        if should_skip_existing(&existing_ids, &id) {
            summary.skipped_existing += 1;
            continue;
        }

        requests.push(AddMemoryRequest {
            id: Some(id.clone()),
            text: item.text.clone(),
            metadata: metadata.clone(),
        });
    }

    if !requests.is_empty() {
        let mut progress = ProgressReporter::new("Adding memories", requests.len());
        manager
            .add_many_with_batch_size_and_progress(requests, options.batch_size, |count| {
                progress.inc(count);
            })
            .await
            .with_context(|| format!("failed to add memories from {}", options.dataset.display()))
            .inspect_err(|_| {
                summary.failed += 1;
                print_add_summary(&summary);
            })?;
        progress.finish();
    }
    if let Some(graph_pipeline) = graph_pipeline {
        build_graph_memories(
            graph_pipeline,
            graph_requests,
            options.cli.graph_build_concurrency,
            options.resume,
        )
        .await?;
    }
    summary.added = summary.total - summary.skipped_existing;

    println!(
        "added {} memories from {}",
        texts.len(),
        options.dataset.display()
    );
    print_add_summary(&summary);
    Ok(())
}

async fn run_add_prepared_memories(
    manager: MemoryManager,
    store: Arc<dyn MemoryStore>,
    graph_pipeline: Option<Arc<GraphBuildPipeline>>,
    json: &serde_json::Value,
    options: AddRunOptions<'_>,
) -> Result<()> {
    let memories = json
        .get("memories")
        .and_then(|value| value.as_array())
        .context("prepared schema v1 field `memories` must be a JSON array")?;

    if memories.is_empty() {
        bail!("prepared schema v1 field `memories` must contain at least one memory");
    }

    let existing_ids = existing_ids_for_resume(&store, options.resume).await?;
    let mut summary = AddSummary {
        total: memories.len(),
        ..Default::default()
    };
    let mut requests = Vec::with_capacity(memories.len());
    let mut graph_requests = Vec::new();
    for (index, memory) in memories.iter().enumerate() {
        let object = memory
            .as_object()
            .with_context(|| format!("memories[{index}] must be a JSON object"))?;
        let text = object
            .get("text")
            .and_then(|value| value.as_str())
            .with_context(|| format!("memories[{index}].text must be a string"))?
            .trim();
        if text.is_empty() {
            bail!("memories[{index}].text must not be empty");
        }

        let id = object
            .get("id")
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .with_context(|| format!("memories[{index}].id must be a string when present"))
            })
            .transpose()?
            .unwrap_or_else(|| format!("$.memories[{index}].text:{index}"));
        let metadata = match object.get("metadata") {
            Some(value) => {
                if !value.is_object() {
                    bail!("memories[{index}].metadata must be a JSON object when present");
                }
                value.clone()
            }
            None => serde_json::Value::Object(Default::default()),
        };
        let path = format!("$.memories[{index}].text");
        let metadata = prepared_memory_metadata(
            metadata,
            &id,
            &options.dataset.display().to_string(),
            &path,
            "text",
        );

        if graph_pipeline.is_some() {
            graph_requests.push(build_graph_add_request(
                options.cli,
                &id,
                text,
                metadata.clone(),
                &path,
                true,
            )?);
        }

        if should_skip_existing(&existing_ids, &id) {
            summary.skipped_existing += 1;
            continue;
        }

        requests.push(AddMemoryRequest {
            id: Some(id.clone()),
            text: text.to_string(),
            metadata: metadata.clone(),
        });
    }

    if !requests.is_empty() {
        let mut progress = ProgressReporter::new("Adding prepared memories", requests.len());
        manager
            .add_many_with_batch_size_and_progress(requests, options.batch_size, |count| {
                progress.inc(count);
            })
            .await
            .with_context(|| "failed to add prepared memories in batch")
            .inspect_err(|_| {
                summary.failed += 1;
                print_add_summary(&summary);
            })?;
        progress.finish();
    }
    if let Some(graph_pipeline) = graph_pipeline {
        build_graph_memories(
            graph_pipeline,
            graph_requests,
            options.cli.graph_build_concurrency,
            options.resume,
        )
        .await?;
    }
    summary.added = summary.total - summary.skipped_existing;

    println!(
        "added {} prepared memories from {}",
        memories.len(),
        options.dataset.display()
    );
    print_add_summary(&summary);
    Ok(())
}

struct SearchRunOptions<'a> {
    output: PathBuf,
    top_k: usize,
    query_fields: &'a [String],
    filter: Option<String>,
    batch_size: usize,
    resume: bool,
}

async fn run_search(
    manager: MemoryManager,
    dataset: Option<PathBuf>,
    query: Option<String>,
    options: SearchRunOptions<'_>,
    cli: &Cli,
) -> Result<()> {
    let cli_filter = parse_filter(options.filter.clone())?;
    let mut queries = Vec::new();
    if let Some(query) = query {
        queries.push(TextItem {
            path: "$.query".to_string(),
            field: "query".to_string(),
            text: query,
            metadata: serde_json::Value::Object(Default::default()),
        });
    }
    if let Some(dataset) = dataset.as_ref() {
        let json = load_json(dataset)?;
        if is_prepared_schema_v1(&json) && json.get("queries").is_some() {
            return run_search_prepared_queries(manager, &json, options, cli_filter, cli).await;
        }
        collect_field_texts(&json, "$", options.query_fields, &mut queries);
    }
    if queries.is_empty() {
        bail!("no queries provided; pass --query or --dataset with query fields");
    }

    let mut outputs = Vec::new();
    if queries.len() == 1 {
        let item = queries.into_iter().next().unwrap();
        let graph_memory_space_id = if cli.graph {
            graph_memory_space_for_query(&item.path, cli_filter.as_ref(), false, cli)?
        } else {
            None
        };
        let results = manager
            .search(SearchMemoryRequest {
                query: item.text.clone(),
                top_k: options.top_k,
                filter: cli_filter.clone(),
                graph_memory_space_id,
                graph_target_subject: None,
                graph_target_evidence_speaker: None,
            })
            .await
            .with_context(|| format!("failed to search query at {}", item.path))?;
        outputs.push(QueryOutput {
            query_path: item.path,
            query: item.text,
            query_id: None,
            filter: cli_filter.clone(),
            metadata: None,
            task: None,
            completed: true,
            results: results
                .into_iter()
                .map(|result| SearchOutput {
                    id: search_output_id(&result.record.id, &result.record.metadata),
                    text: result.record.text,
                    metadata: result.record.metadata,
                    score: result.score,
                })
                .collect(),
        });
    } else if !queries.is_empty() {
        let search_requests: Vec<SearchMemoryRequest> = queries
            .iter()
            .map(|item| {
                Ok(SearchMemoryRequest {
                    query: item.text.clone(),
                    top_k: options.top_k,
                    filter: cli_filter.clone(),
                    graph_memory_space_id: if cli.graph {
                        graph_memory_space_for_query(&item.path, cli_filter.as_ref(), false, cli)?
                    } else {
                        None
                    },
                    graph_target_subject: None,
                    graph_target_evidence_speaker: None,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let mut progress = ProgressReporter::new("Searching queries", search_requests.len());
        let batch_results = manager
            .search_many_with_batch_size_and_progress(
                search_requests,
                options.batch_size,
                |count| {
                    progress.inc(count);
                },
            )
            .await
            .with_context(|| "failed to batch search queries")?;
        progress.finish();
        outputs = queries
            .into_iter()
            .zip(batch_results)
            .map(|(item, results)| QueryOutput {
                query_path: item.path,
                query: item.text,
                query_id: None,
                filter: cli_filter.clone(),
                metadata: None,
                task: None,
                completed: true,
                results: results
                    .into_iter()
                    .map(|result| SearchOutput {
                        id: search_output_id(&result.record.id, &result.record.metadata),
                        text: result.record.text,
                        metadata: result.record.metadata,
                        score: result.score,
                    })
                    .collect(),
            })
            .collect();
    }

    write_atomic_json(&options.output, &outputs).await?;
    println!("wrote search results to {}", options.output.display());
    Ok(())
}

async fn run_search_prepared_queries(
    manager: MemoryManager,
    json: &serde_json::Value,
    options: SearchRunOptions<'_>,
    cli_filter: Option<serde_json::Value>,
    cli: &Cli,
) -> Result<()> {
    let queries = json
        .get("queries")
        .and_then(|value| value.as_array())
        .context("prepared schema v1 field `queries` must be a JSON array")?;

    if queries.is_empty() {
        bail!("prepared schema v1 field `queries` must contain at least one query");
    }

    let mut output_templates = Vec::with_capacity(queries.len());
    let mut search_requests = Vec::with_capacity(queries.len());
    for (index, query) in queries.iter().enumerate() {
        let object = query
            .as_object()
            .with_context(|| format!("queries[{index}] must be a JSON object"))?;
        let query_text = object
            .get("text")
            .and_then(|value| value.as_str())
            .with_context(|| format!("queries[{index}].text must be a string"))?
            .trim();
        if query_text.is_empty() {
            bail!("queries[{index}].text must not be empty");
        }

        let query_id = object
            .get("id")
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .with_context(|| format!("queries[{index}].id must be a string when present"))
            })
            .transpose()?;
        let query_filter = optional_object_field(object, "filter", &format!("queries[{index}]"))?;
        let metadata = optional_object_field(object, "metadata", &format!("queries[{index}]"))?;
        let task = optional_object_field(object, "task", &format!("queries[{index}]"))?;
        let effective_filter = query_filter.clone().or_else(|| cli_filter.clone());

        search_requests.push(SearchMemoryRequest {
            query: query_text.to_string(),
            top_k: options.top_k,
            filter: effective_filter.clone(),
            graph_memory_space_id: if cli.graph {
                graph_memory_space_for_query(
                    &format!("$.queries[{index}].text"),
                    effective_filter.as_ref(),
                    true,
                    cli,
                )?
            } else {
                None
            },
            graph_target_subject: None,
            graph_target_evidence_speaker: None,
        });
        output_templates.push(QueryOutput {
            query_path: format!("$.queries[{index}].text"),
            query: query_text.to_string(),
            query_id,
            filter: effective_filter,
            metadata,
            task,
            completed: false,
            results: Vec::new(),
        });
    }

    let mut outputs = output_templates.clone();
    let mut completed: Vec<usize> = Vec::new();
    if options.resume {
        if let Some(existing) = load_existing_outputs(&options.output) {
            completed = resume_completed_indexes(&outputs, &existing);
            for index in &completed {
                if let Some(row) = existing.get(*index) {
                    outputs[*index] = row.clone();
                }
            }
            if !completed.is_empty() {
                println!(
                    "search --resume recovered {} of {} completed queries",
                    completed.len(),
                    outputs.len()
                );
            }
        }
    }

    let pending: Vec<usize> = (0..outputs.len())
        .filter(|index| !completed.contains(index))
        .collect();
    let mut progress = ProgressReporter::new("Searching prepared queries", pending.len());
    let batch_size = options.batch_size.max(1);
    for chunk in pending.chunks(batch_size) {
        let chunk_requests: Vec<SearchMemoryRequest> = chunk
            .iter()
            .map(|index| search_requests[*index].clone())
            .collect();
        let chunk_results = manager
            .search_many_with_batch_size_and_progress(chunk_requests, batch_size, |count| {
                progress.inc(count);
            })
            .await
            .with_context(|| "failed to search prepared queries in batch")?;
        for (offset, results) in chunk_results.into_iter().enumerate() {
            let index = chunk[offset];
            outputs[index].results = results
                .into_iter()
                .map(|result| SearchOutput {
                    id: search_output_id(&result.record.id, &result.record.metadata),
                    text: result.record.text,
                    metadata: result.record.metadata,
                    score: result.score,
                })
                .collect();
            outputs[index].completed = true;
        }
        write_atomic_json(&options.output, &outputs).await?;
    }
    progress.finish();
    write_atomic_json(&options.output, &outputs).await?;
    println!("wrote search results to {}", options.output.display());
    Ok(())
}

/// Load previously written search outputs for `--resume`. Returns `None` when the
/// output file is absent or cannot be parsed: a half-written file from a killed
/// process must not abort the run, it simply restarts search from scratch.
fn load_existing_outputs(output: &Path) -> Option<Vec<QueryOutput>> {
    let bytes = std::fs::read(output).ok()?;
    serde_json::from_slice::<Vec<QueryOutput>>(&bytes).ok()
}

/// Indexes of queries already present in `existing`, matched by `query_id` when
/// both sides have one, otherwise by the `$.queries[i].text` path embedded in
/// `query_path`. A query counts as completed only when its existing row has at
/// least one result, so an empty (interrupted) row is re-searched.
fn resume_completed_indexes(templates: &[QueryOutput], existing: &[QueryOutput]) -> Vec<usize> {
    let by_query_id: HashMap<String, usize> = existing
        .iter()
        .enumerate()
        .filter_map(|(index, row)| row.query_id.as_ref().map(|id| (id.clone(), index)))
        .collect();
    let by_path: HashMap<String, usize> = existing
        .iter()
        .enumerate()
        .map(|(index, row)| (row.query_path.clone(), index))
        .collect();
    let mut completed = Vec::new();
    for (index, template) in templates.iter().enumerate() {
        let matched = template
            .query_id
            .as_ref()
            .and_then(|id| by_query_id.get(id).copied())
            .or_else(|| by_path.get(&template.query_path).copied());
        if let Some(existing_index) = matched {
            if existing[existing_index].completed || !existing[existing_index].results.is_empty() {
                completed.push(index);
            }
        }
    }
    completed
}

/// Write `outputs` to `output` atomically: serialize to a `.tmp` sibling then
/// rename over the target, so a crash mid-write cannot leave a truncated file.
async fn write_atomic_json(output: &Path, outputs: &[QueryOutput]) -> Result<()> {
    if let Some(parent) = output.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let bytes = serde_json::to_vec_pretty(outputs)?;
    let temporary = output.with_extension("tmp");
    tokio::fs::write(&temporary, &bytes).await?;
    tokio::fs::rename(&temporary, output).await?;
    Ok(())
}

struct ProgressReporter {
    label: &'static str,
    total: usize,
    done: usize,
    enabled: bool,
    started: Instant,
    last_log: Instant,
    next_log_at: usize,
    last_render_len: usize,
    last_log_done: usize,
}

impl ProgressReporter {
    fn new(label: &'static str, total: usize) -> Self {
        let now = Instant::now();
        let enabled = io::stderr().is_terminal() && total > 0;
        let mut reporter = Self {
            label,
            total,
            done: 0,
            enabled,
            started: now,
            last_log: now,
            next_log_at: progress_log_interval(total),
            last_render_len: 0,
            last_log_done: 0,
        };
        reporter.render();
        reporter
    }

    fn inc(&mut self, count: usize) {
        self.done = (self.done + count).min(self.total);
        if self.enabled {
            self.render();
            return;
        }

        let now = Instant::now();
        if self.done >= self.total
            || self.done >= self.next_log_at
            || now.duration_since(self.last_log) >= Duration::from_secs(30)
        {
            self.log_line();
            self.last_log = now;
            self.last_log_done = self.done;
            self.next_log_at = self.done + progress_log_interval(self.total);
        }
    }

    fn finish(&mut self) {
        self.done = self.total;
        if self.enabled {
            self.render();
            eprintln!();
        } else if self.total > 0 && self.last_log_done != self.total {
            self.log_line();
        }
    }

    fn render(&mut self) {
        if !self.enabled {
            return;
        }
        let percent = progress_percent(self.done, self.total);
        let elapsed = self.started.elapsed().as_secs_f32();
        let rate = if elapsed > 0.0 {
            self.done as f32 / elapsed
        } else {
            0.0
        };
        let filled = (percent / 5.0).round() as usize;
        let filled = filled.min(20);
        let bar = format!("{}{}", "#".repeat(filled), "-".repeat(20 - filled));
        let line = format!(
            "{}: {:>3.0}%|{}| {}/{} [{:.1}s, {:.1}/s]",
            self.label, percent, bar, self.done, self.total, elapsed, rate
        );
        let padding = self.last_render_len.saturating_sub(line.len());
        eprint!("\r{}{}", line, " ".repeat(padding));
        self.last_render_len = line.len();
        let _ = io::stderr().flush();
    }

    fn log_line(&self) {
        println!(
            "[{}] {}/{} done | elapsed={}s",
            self.label,
            self.done,
            self.total,
            self.started.elapsed().as_secs()
        );
    }
}

fn progress_log_interval(total: usize) -> usize {
    if total <= 100 {
        10
    } else if total <= 1000 {
        50
    } else {
        100
    }
}

fn progress_percent(done: usize, total: usize) -> f32 {
    if total == 0 {
        100.0
    } else {
        (done as f32 / total as f32) * 100.0
    }
}

fn load_json(path: &PathBuf) -> Result<serde_json::Value> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read dataset {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("failed to parse dataset {}", path.display()))
}

async fn existing_ids_for_resume(
    store: &Arc<dyn MemoryStore>,
    resume: bool,
) -> Result<Option<HashSet<String>>> {
    if !resume {
        return Ok(None);
    }
    let records = store
        .list_records()
        .await
        .context("failed to read existing memory store for --resume")?;
    Ok(Some(records.into_iter().map(|record| record.id).collect()))
}

fn should_skip_existing(existing_ids: &Option<HashSet<String>>, id: &str) -> bool {
    existing_ids.as_ref().is_some_and(|ids| ids.contains(id))
}

#[derive(Default)]
struct AddSummary {
    total: usize,
    skipped_existing: usize,
    added: usize,
    failed: usize,
}

fn print_add_summary(summary: &AddSummary) {
    println!(
        "add summary: total={}, skipped_existing={}, added={}, failed={}",
        summary.total, summary.skipped_existing, summary.added, summary.failed
    );
}

async fn build_graph_memories(
    graph_pipeline: Arc<GraphBuildPipeline>,
    requests: Vec<GraphAddMemoryRequest>,
    concurrency: usize,
    resume: bool,
) -> Result<()> {
    if requests.is_empty() {
        return Ok(());
    }
    let mut progress = ProgressReporter::new("Building graph memories", requests.len());
    let mut requests = requests.into_iter();
    let mut in_flight = JoinSet::new();
    for _ in 0..concurrency {
        let Some(request) = requests.next() else {
            break;
        };
        spawn_graph_build(&mut in_flight, graph_pipeline.clone(), request, resume);
    }

    while let Some(result) = in_flight.join_next().await {
        let (source_ref, result) = result.context("graph build task panicked")?;
        if let Err(error) = result {
            in_flight.abort_all();
            while in_flight.join_next().await.is_some() {}
            return Err(error)
                .with_context(|| format!("failed to build graph memory for {source_ref:?}"));
        }
        progress.inc(1);
        if let Some(request) = requests.next() {
            spawn_graph_build(&mut in_flight, graph_pipeline.clone(), request, resume);
        }
    }
    progress.finish();
    Ok(())
}

fn spawn_graph_build(
    tasks: &mut JoinSet<(
        Option<String>,
        memory_core::MemoryResult<Option<memory_core::GraphBuildResult>>,
    )>,
    graph_pipeline: Arc<GraphBuildPipeline>,
    request: GraphAddMemoryRequest,
    resume: bool,
) {
    tasks.spawn(async move {
        let source_ref = request.source_ref.clone();
        let result = if resume {
            graph_pipeline.resume_memory(request).await
        } else {
            graph_pipeline.build_memory_if_needed(request).await
        };
        (source_ref, result)
    });
}

fn build_graph_add_request(
    cli: &Cli,
    id: &str,
    text: &str,
    metadata: serde_json::Value,
    path: &str,
    is_prepared: bool,
) -> Result<GraphAddMemoryRequest> {
    let memory_space_id = graph_memory_space_for_memory(path, &metadata, is_prepared, cli)?
        .with_context(|| {
            format!(
                "graph memory space could not be derived for memory `{id}` at `{path}`; \
                 set --graph-memory-space-mode/--graph-memory-space-field explicitly"
            )
        })?;
    Ok(GraphAddMemoryRequest {
        memory_space_id,
        owner_id: cli.graph_owner_id.clone(),
        idempotency_key: id.to_string(),
        text: text.to_string(),
        metadata: metadata.clone(),
        session_id: metadata
            .get("session_id")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        session_sequence: metadata.get("turn_index").and_then(|value| value.as_i64()),
        source_kind: "benchmark".to_string(),
        source_ref: Some(id.to_string()),
        content_role: "message".to_string(),
        created_by_agent_id: None,
        observed_at_ms: metadata
            .get("observed_at_ms")
            .and_then(|value| value.as_u64()),
    })
}

fn parse_filter(raw: Option<String>) -> Result<Option<serde_json::Value>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let filter: serde_json::Value =
        serde_json::from_str(&raw).with_context(|| format!("invalid --filter JSON: {raw}"))?;
    if !filter.is_object() {
        bail!("--filter must be a JSON object, for example: {{\"shared_context_id\":\"...\"}}");
    }
    Ok(Some(filter))
}

fn graph_memory_space_for_memory(
    path: &str,
    metadata: &serde_json::Value,
    is_prepared: bool,
    cli: &Cli,
) -> Result<Option<String>> {
    match cli.graph_memory_space_mode {
        GraphMemorySpaceMode::MetadataField => {
            metadata_string_field(metadata, &cli.graph_memory_space_field)
        }
        GraphMemorySpaceMode::PathPrefix => Ok(top_level_array_space(path)),
        GraphMemorySpaceMode::Auto => {
            if is_prepared {
                metadata_string_field(metadata, &cli.graph_memory_space_field)
            } else {
                Ok(top_level_array_space(path))
            }
        }
    }
}

fn graph_memory_space_for_query(
    path: &str,
    filter: Option<&serde_json::Value>,
    is_prepared: bool,
    cli: &Cli,
) -> Result<Option<String>> {
    match cli.graph_memory_space_mode {
        GraphMemorySpaceMode::MetadataField => filter
            .map(|value| metadata_string_field(value, &cli.graph_memory_space_field))
            .unwrap_or(Ok(None)),
        GraphMemorySpaceMode::PathPrefix => Ok(top_level_array_space(path)),
        GraphMemorySpaceMode::Auto => {
            if is_prepared {
                filter
                    .map(|value| metadata_string_field(value, &cli.graph_memory_space_field))
                    .unwrap_or(Ok(None))
            } else if let Some(memory_space_id) = top_level_array_space(path) {
                Ok(Some(memory_space_id))
            } else {
                filter
                    .map(|value| metadata_string_field(value, &cli.graph_memory_space_field))
                    .unwrap_or(Ok(None))
            }
        }
    }
}

fn metadata_string_field(value: &serde_json::Value, field: &str) -> Result<Option<String>> {
    let Some(raw) = value.get(field) else {
        return Ok(None);
    };
    let Some(text) = raw.as_str() else {
        bail!("graph memory space field `{field}` must be a string");
    };
    let text = text.trim();
    Ok((!text.is_empty()).then(|| text.to_string()))
}

fn top_level_array_space(path: &str) -> Option<String> {
    let rest = path.strip_prefix("$[")?;
    let end = rest.find(']')?;
    let index = &rest[..end];
    if index.parse::<usize>().is_ok() {
        Some(format!("path:$[{index}]"))
    } else {
        None
    }
}

fn search_output_id(record_id: &str, metadata: &serde_json::Value) -> String {
    metadata
        .get("benchmark_memory_id")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| record_id.to_string())
}

fn prepared_memory_metadata(
    mut metadata: serde_json::Value,
    memory_id: &str,
    dataset: &str,
    path: &str,
    field: &str,
) -> serde_json::Value {
    insert_default_metadata(
        &mut metadata,
        "benchmark_memory_id",
        serde_json::Value::String(memory_id.to_string()),
    );
    insert_default_metadata(
        &mut metadata,
        "dataset",
        serde_json::Value::String(dataset.to_string()),
    );
    insert_default_metadata(
        &mut metadata,
        "path",
        serde_json::Value::String(path.to_string()),
    );
    insert_default_metadata(
        &mut metadata,
        "field",
        serde_json::Value::String(field.to_string()),
    );
    metadata
}

fn is_prepared_schema_v1(json: &serde_json::Value) -> bool {
    json.get("schema_version").and_then(|value| value.as_str()) == Some(PREPARED_SCHEMA_VERSION)
}

fn optional_object_field(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<Option<serde_json::Value>> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    if !value.is_object() {
        bail!("{context}.{field} must be a JSON object when present");
    }
    Ok(Some(value.clone()))
}

#[derive(Clone, Debug)]
struct TextItem {
    path: String,
    field: String,
    text: String,
    metadata: serde_json::Value,
}

fn collect_field_texts(
    value: &serde_json::Value,
    path: &str,
    fields: &[String],
    output: &mut Vec<TextItem>,
) {
    match value {
        serde_json::Value::Object(object) => {
            for field in fields {
                if let Some(text) = object.get(field).and_then(|value| value.as_str()) {
                    let text = text.trim();
                    if !text.is_empty() {
                        let item_path = format!("{path}.{field}");
                        output.push(TextItem {
                            metadata: collect_item_metadata(object, &item_path),
                            path: item_path,
                            field: field.clone(),
                            text: text.to_string(),
                        });
                    }
                }
            }
            for (key, child) in object {
                collect_field_texts(child, &format!("{path}.{key}"), fields, output);
            }
        }
        serde_json::Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_field_texts(child, &format!("{path}[{index}]"), fields, output);
            }
        }
        _ => {}
    }
}

fn collect_item_metadata(
    object: &serde_json::Map<String, serde_json::Value>,
    item_path: &str,
) -> serde_json::Value {
    let mut metadata = serde_json::Map::new();
    if let Some(conversation_index) = parse_conversation_index(item_path) {
        if let Some(shared_context_id) = object.get("shared_context_id") {
            metadata.insert("shared_context_id".to_string(), shared_context_id.clone());
        }
        if let Some(speaker) = object.get("speaker") {
            metadata.insert("speaker".to_string(), speaker.clone());
        }
        metadata.insert(
            "conversation_index".to_string(),
            serde_json::Value::Number(conversation_index.into()),
        );
    }
    serde_json::Value::Object(metadata)
}

fn parse_conversation_index(path: &str) -> Option<u64> {
    let prefix = "$.conversation[";
    let rest = path.strip_prefix(prefix)?;
    let end = rest.find(']')?;
    rest[..end].parse().ok()
}

/// Merge `extra` into `target`. If a key exists in both, `extra` wins.
fn merge_metadata(target: &mut serde_json::Value, extra: &serde_json::Value) {
    let Some(target_object) = target.as_object_mut() else {
        return;
    };
    let Some(extra_object) = extra.as_object() else {
        return;
    };
    for (key, value) in extra_object {
        target_object.insert(key.clone(), value.clone());
    }
}

fn insert_default_metadata(target: &mut serde_json::Value, key: &str, value: serde_json::Value) {
    let Some(target_object) = target.as_object_mut() else {
        return;
    };
    target_object.entry(key.to_string()).or_insert(value);
}

#[derive(Clone, Serialize, Deserialize)]
struct QueryOutput {
    query_path: String,
    query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    query_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    filter: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task: Option<serde_json::Value>,
    #[serde(default)]
    completed: bool,
    results: Vec<SearchOutput>,
}

#[derive(Clone, Serialize, Deserialize)]
struct SearchOutput {
    id: String,
    text: String,
    metadata: serde_json::Value,
    score: f32,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use memory_core::graph::{
        ExtractedEntityCandidate, ExtractedFactCandidate, GraphEvidenceSpan, GraphExtractionInput,
        GraphExtractionOutput, GraphExtractor,
    };

    use super::*;

    #[derive(Debug)]
    struct BenchGraphExtractor;

    #[async_trait::async_trait]
    impl GraphExtractor for BenchGraphExtractor {
        fn extractor_name(&self) -> &str {
            "bench-test-extractor"
        }

        fn model_name(&self) -> &str {
            "bench-test-model"
        }

        fn prompt_version(&self) -> &str {
            "bench-test-prompt-v1"
        }

        fn schema_version(&self) -> &str {
            "bench-test-schema-v1"
        }

        async fn extract(
            &self,
            input: GraphExtractionInput,
        ) -> memory_core::MemoryResult<GraphExtractionOutput> {
            Ok(GraphExtractionOutput {
                entities: vec![
                    ExtractedEntityCandidate {
                        local_id: "entity:alice".to_string(),
                        name: "Alice".to_string(),
                        entity_type: "PERSON".to_string(),
                        confidence: Some(1.0),
                    },
                    ExtractedEntityCandidate {
                        local_id: "entity:shanghai".to_string(),
                        name: "Shanghai".to_string(),
                        entity_type: "LOCATION".to_string(),
                        confidence: Some(1.0),
                    },
                ],
                facts: vec![ExtractedFactCandidate {
                    local_id: "fact:alice-shanghai".to_string(),
                    subject_ref: "entity:alice".to_string(),
                    predicate: "LIVES_IN".to_string(),
                    object_ref: "entity:shanghai".to_string(),
                    fact_text: "Alice lives in Shanghai.".to_string(),
                    evidence: vec![GraphEvidenceSpan {
                        text: Some(input.text),
                        start_byte: None,
                        end_byte: None,
                    }],
                    confidence: Some(1.0),
                    temporal_expression: None,
                    valid_from_ms: None,
                    valid_to_ms: None,
                }],
                input_tokens: Some(1),
                output_tokens: Some(1),
            })
        }
    }

    #[test]
    fn cli_defaults_to_sqlite_hybrid_search() {
        let cli = Cli::try_parse_from([
            "memory-bench",
            "--embedding",
            "hash",
            "add",
            "--dataset",
            "data/test.json",
        ])
        .expect("parse default CLI");

        assert!(matches!(cli.store_backend, StoreBackendKind::Sqlite));
        assert_eq!(cli.store, PathBuf::from("data/memory.sqlite"));
        assert!(matches!(cli.search_mode, SearchModeKind::Hybrid));
        assert_eq!(cli.embedding_weight, 0.7);
        assert_eq!(cli.bm25_weight, 0.3);
        assert_eq!(cli.candidate_k, None);
        assert!(!cli.rerank);
        assert!(matches!(
            cli.rerank_provider,
            RerankProviderKind::Openrouter
        ));
        assert_eq!(cli.rerank_model, "cohere/rerank-v3.5");
        assert_eq!(cli.rerank_api_key_env, "OPENROUTER_API_KEY");
        assert_eq!(cli.rerank_base_url, "https://openrouter.ai/api/v1");
        assert_eq!(cli.rerank_input_k, 40);
        assert_eq!(cli.rerank_timeout_ms, None);
        assert!(!cli.rerank_fail_open);
    }

    #[test]
    fn cli_parses_sqlite_store_backend() {
        let cli = Cli::try_parse_from([
            "memory-bench",
            "--store-backend",
            "sqlite",
            "--store",
            "data/memory.sqlite",
            "--embedding",
            "hash",
            "add",
            "--dataset",
            "data/test.json",
        ])
        .expect("parse sqlite backend CLI");

        assert!(matches!(cli.store_backend, StoreBackendKind::Sqlite));
        assert_eq!(cli.store, PathBuf::from("data/memory.sqlite"));
    }

    #[test]
    fn cli_parses_search_mode_and_weights() {
        let cli = Cli::try_parse_from([
            "memory-bench",
            "--embedding",
            "hash",
            "--search-mode",
            "hybrid",
            "--embedding-weight",
            "0.6",
            "--bm25-weight",
            "0.4",
            "--candidate-k",
            "42",
            "search",
            "--query",
            "Pacific melodies",
            "--output",
            "outputs/search.json",
        ])
        .expect("parse retrieval CLI");

        assert!(matches!(cli.search_mode, SearchModeKind::Hybrid));
        assert_eq!(cli.embedding_weight, 0.6);
        assert_eq!(cli.bm25_weight, 0.4);
        assert_eq!(cli.candidate_k, Some(42));
    }

    #[test]
    fn cli_parses_graph_search_mode() {
        let cli = Cli::try_parse_from([
            "memory-bench",
            "--embedding",
            "hash",
            "--graph",
            "--search-mode",
            "graph",
            "search",
            "--query",
            "Where does Alice live?",
            "--output",
            "outputs/search.json",
        ])
        .expect("parse graph-only retrieval CLI");

        assert!(matches!(cli.search_mode, SearchModeKind::Graph));
        assert!(cli.graph);
    }

    #[test]
    fn cli_parses_rerank_options() {
        let cli = Cli::try_parse_from([
            "memory-bench",
            "--embedding",
            "hash",
            "--rerank",
            "--rerank-provider",
            "openrouter",
            "--rerank-model",
            "cohere/rerank-v3.5",
            "--rerank-api-key-env",
            "OPENROUTER_API_KEY",
            "--rerank-base-url",
            "https://openrouter.ai/api/v1",
            "--rerank-input-k",
            "40",
            "--rerank-timeout-ms",
            "2500",
            "--rerank-fail-open",
            "search",
            "--query",
            "Pacific melodies",
            "--output",
            "outputs/search.json",
        ])
        .expect("parse rerank CLI");

        assert!(cli.rerank);
        assert!(matches!(
            cli.rerank_provider,
            RerankProviderKind::Openrouter
        ));
        assert_eq!(cli.rerank_model, "cohere/rerank-v3.5");
        assert_eq!(cli.rerank_api_key_env, "OPENROUTER_API_KEY");
        assert_eq!(cli.rerank_base_url, "https://openrouter.ai/api/v1");
        assert_eq!(cli.rerank_input_k, 40);
        assert_eq!(cli.rerank_timeout_ms, Some(2500));
        assert!(cli.rerank_fail_open);
    }

    #[test]
    fn cli_parses_graph_options() {
        let cli = Cli::try_parse_from([
            "memory-bench",
            "--embedding",
            "hash",
            "--graph-build",
            "--graph",
            "--graph-weight",
            "0.4",
            "--graph-fail-open",
            "--graph-memory-space-mode",
            "path-prefix",
            "--graph-memory-space-field",
            "tenant",
            "--graph-owner-id",
            "bench-owner",
            "--graph-llm-api-key-env",
            "GRAPH_KEY",
            "--graph-llm-model",
            "openai/gpt-4o-mini",
            "--graph-llm-base-url",
            "https://openrouter.ai/api/v1",
            "--graph-llm-timeout-ms",
            "1000",
            "--graph-build-concurrency",
            "4",
            "search",
            "--query",
            "Where does Alice live?",
            "--output",
            "outputs/search.json",
        ])
        .expect("parse graph CLI");

        assert!(cli.graph_build);
        assert!(cli.graph);
        assert_eq!(cli.graph_weight, 0.4);
        assert!(cli.graph_fail_open);
        assert!(matches!(
            cli.graph_memory_space_mode,
            GraphMemorySpaceMode::PathPrefix
        ));
        assert_eq!(cli.graph_memory_space_field, "tenant");
        assert_eq!(cli.graph_owner_id, "bench-owner");
        assert_eq!(cli.graph_llm_api_key_env, "GRAPH_KEY");
        assert_eq!(cli.graph_llm_model, "openai/gpt-4o-mini");
        assert_eq!(cli.graph_llm_base_url, "https://openrouter.ai/api/v1");
        assert_eq!(cli.graph_llm_timeout_ms, Some(1000));
        assert_eq!(cli.graph_build_concurrency, 4);
    }

    #[test]
    fn cli_rejects_zero_graph_build_concurrency() {
        let result = Cli::try_parse_from([
            "memory-bench",
            "--embedding",
            "hash",
            "--graph-build-concurrency",
            "0",
            "add",
            "--dataset",
            "data/test.json",
        ]);
        let error = match result {
            Ok(_) => panic!("zero concurrency must be rejected"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("must be at least 1"));
    }

    #[test]
    fn graph_memory_space_auto_uses_prepared_scope_id() {
        let cli = graph_test_cli();
        let metadata = serde_json::json!({"scope_id": "scope-a"});
        let filter = serde_json::json!({"scope_id": "scope-a"});

        assert_eq!(
            graph_memory_space_for_memory("$.memories[0].text", &metadata, true, &cli)
                .expect("memory space"),
            Some("scope-a".to_string())
        );
        assert_eq!(
            graph_memory_space_for_query("$.queries[0].text", Some(&filter), true, &cli)
                .expect("query space"),
            Some("scope-a".to_string())
        );
    }

    #[test]
    fn graph_memory_space_auto_uses_top_level_path_for_raw_array_shape() {
        let cli = graph_test_cli();
        let filter = serde_json::json!({"scope_id": "scope-a"});
        assert_eq!(
            graph_memory_space_for_memory(
                "$[12].messages[0].text",
                &serde_json::json!({}),
                false,
                &cli,
            )
            .expect("memory space"),
            Some("path:$[12]".to_string())
        );
        assert_eq!(
            graph_memory_space_for_query("$[12].queries[0].text", Some(&filter), false, &cli)
                .expect("query space"),
            Some("path:$[12]".to_string())
        );
    }

    #[test]
    fn graph_memory_space_auto_uses_filter_for_ad_hoc_query() {
        let cli = graph_test_cli();
        let filter = serde_json::json!({"scope_id": "scope-a"});

        assert_eq!(
            graph_memory_space_for_query("$.query", Some(&filter), false, &cli)
                .expect("query space"),
            Some("scope-a".to_string())
        );
    }

    #[test]
    fn graph_memory_space_metadata_field_uses_configured_field() {
        let cli = Cli::try_parse_from([
            "memory-bench",
            "--embedding",
            "hash",
            "--graph-memory-space-mode",
            "metadata-field",
            "--graph-memory-space-field",
            "tenant_id",
            "search",
            "--query",
            "Where?",
            "--output",
            "outputs/search.json",
        ])
        .expect("parse graph memory space CLI");
        let metadata = serde_json::json!({"tenant_id": "tenant-a"});

        assert_eq!(
            graph_memory_space_for_memory("$.memory.text", &metadata, false, &cli)
                .expect("memory space"),
            Some("tenant-a".to_string())
        );
        assert_eq!(
            graph_memory_space_for_query("$.query", Some(&metadata), false, &cli)
                .expect("query space"),
            Some("tenant-a".to_string())
        );
    }

    #[test]
    fn graph_memory_space_path_prefix_ignores_metadata_field() {
        let cli = Cli::try_parse_from([
            "memory-bench",
            "--embedding",
            "hash",
            "--graph-memory-space-mode",
            "path-prefix",
            "--graph-memory-space-field",
            "tenant_id",
            "search",
            "--query",
            "Where?",
            "--output",
            "outputs/search.json",
        ])
        .expect("parse graph memory space CLI");
        let metadata = serde_json::json!({"tenant_id": "tenant-a"});

        assert_eq!(
            graph_memory_space_for_memory("$[3].memory.text", &metadata, false, &cli)
                .expect("memory space"),
            Some("path:$[3]".to_string())
        );
        assert_eq!(
            graph_memory_space_for_query("$[3].qa[0].question", Some(&metadata), false, &cli)
                .expect("query space"),
            Some("path:$[3]".to_string())
        );
    }

    #[test]
    fn build_graph_add_request_requires_memory_space_id() {
        let cli = graph_test_cli();
        let error = build_graph_add_request(
            &cli,
            "record-1",
            "Alice lives in Shanghai.",
            serde_json::json!({}),
            "$.memory.text",
            false,
        )
        .expect_err("missing graph memory space should fail");

        assert!(format!("{error}").contains(
            "graph memory space could not be derived for memory `record-1` at `$.memory.text`"
        ));
    }

    #[test]
    fn graph_add_request_uses_observed_at_metadata() {
        let cli = graph_test_cli();

        let request = build_graph_add_request(
            &cli,
            "turn-1",
            "I went to a LGBTQ support group yesterday.",
            serde_json::json!({
                "scope_id": "scope-a",
                "session_id": "session_2",
                "turn_index": 0,
                "observed_at_ms": 1_683_554_160_000u64,
            }),
            "$.memories[0].text",
            true,
        )
        .expect("graph request");

        assert_eq!(request.memory_space_id, "scope-a");
        assert_eq!(request.session_id.as_deref(), Some("session_2"));
        assert_eq!(request.session_sequence, Some(0));
        assert_eq!(request.observed_at_ms, Some(1_683_554_160_000));
    }

    #[test]
    fn search_output_id_prefers_benchmark_memory_id() {
        assert_eq!(
            search_output_id(
                "graph-record-id",
                &serde_json::json!({"benchmark_memory_id": "turn-1"})
            ),
            "turn-1"
        );
        assert_eq!(
            search_output_id("record-id", &serde_json::json!({})),
            "record-id"
        );
    }

    #[test]
    fn prepared_memory_metadata_preserves_benchmark_memory_id() {
        let metadata = prepared_memory_metadata(
            serde_json::json!({"scope_id": "scope-a"}),
            "turn-1",
            "data/prepared.json",
            "$.memories[0].text",
            "text",
        );

        assert_eq!(metadata["benchmark_memory_id"], "turn-1");
        assert_eq!(metadata["scope_id"], "scope-a");
        assert_eq!(metadata["dataset"], "data/prepared.json");
        assert_eq!(metadata["path"], "$.memories[0].text");
        assert_eq!(metadata["field"], "text");
    }

    #[test]
    fn graph_search_request_uses_scope_id_for_prepared_query() {
        let cli = graph_enabled_test_cli();
        let filter = serde_json::json!({"scope_id": "scope-a"});

        assert_eq!(
            graph_memory_space_for_query("$.queries[0].text", Some(&filter), true, &cli)
                .expect("graph space"),
            Some("scope-a".to_string())
        );
    }

    #[test]
    fn build_runtime_does_not_require_rerank_api_key_when_rerank_is_disabled() {
        let cli = Cli::try_parse_from([
            "memory-bench",
            "--embedding",
            "hash",
            "--rerank-api-key-env",
            "RAM_A_TEST_MISSING_RERANK_KEY_DISABLED",
            "add",
            "--dataset",
            "data/test.json",
        ])
        .expect("parse CLI");

        let _runtime = build_runtime(&cli).expect("runtime without rerank API key");
    }

    #[test]
    fn build_runtime_requires_rerank_api_key_when_rerank_is_enabled() {
        let cli = Cli::try_parse_from([
            "memory-bench",
            "--embedding",
            "hash",
            "--rerank",
            "--rerank-api-key-env",
            "RAM_A_TEST_MISSING_RERANK_KEY_ENABLED",
            "add",
            "--dataset",
            "data/test.json",
        ])
        .expect("parse CLI");

        let error = match build_runtime(&cli) {
            Ok(_) => panic!("missing rerank API key should fail"),
            Err(error) => error,
        };

        assert!(format!("{error}")
            .contains("missing rerank API key env RAM_A_TEST_MISSING_RERANK_KEY_ENABLED"));
    }

    #[test]
    fn cli_parses_search_resume() {
        let cli = Cli::try_parse_from([
            "memory-bench",
            "--embedding",
            "hash",
            "search",
            "--dataset",
            "data/test.json",
            "--output",
            "outputs/search.json",
            "--resume",
        ])
        .expect("parse search --resume");

        match cli.command {
            Command::Search { resume, .. } => assert!(resume),
            _ => panic!("expected Search command"),
        }
    }

    #[test]
    fn search_resume_defaults_to_false() {
        let cli = Cli::try_parse_from([
            "memory-bench",
            "--embedding",
            "hash",
            "search",
            "--query",
            "q",
            "--output",
            "outputs/search.json",
        ])
        .expect("parse search without --resume");

        match cli.command {
            Command::Search { resume, .. } => assert!(!resume),
            _ => panic!("expected Search command"),
        }
    }

    #[test]
    fn resume_completed_indexes_skip_queries_with_matching_ids() {
        let templates = vec![
            QueryOutput {
                query_path: "$.queries[0].text".to_string(),
                query: "q0".to_string(),
                query_id: Some("Q0".to_string()),
                filter: None,
                metadata: None,
                task: None,
                completed: false,
                results: Vec::new(),
            },
            QueryOutput {
                query_path: "$.queries[1].text".to_string(),
                query: "q1".to_string(),
                query_id: Some("Q1".to_string()),
                filter: None,
                metadata: None,
                task: None,
                completed: false,
                results: Vec::new(),
            },
            QueryOutput {
                query_path: "$.queries[2].text".to_string(),
                query: "q2".to_string(),
                query_id: Some("Q2".to_string()),
                filter: None,
                metadata: None,
                task: None,
                completed: false,
                results: Vec::new(),
            },
        ];
        let existing = vec![QueryOutput {
            query_path: "$.queries[0].text".to_string(),
            query: "q0".to_string(),
            query_id: Some("Q0".to_string()),
            filter: None,
            metadata: None,
            task: None,
            completed: true,
            results: vec![SearchOutput {
                id: "m0".to_string(),
                text: "hit".to_string(),
                metadata: serde_json::Value::Null,
                score: 0.9,
            }],
        }];

        let completed = resume_completed_indexes(&templates, &existing);

        assert_eq!(completed, vec![0]);
    }

    #[test]
    fn resume_completed_indexes_falls_back_to_path_index_without_query_id() {
        let templates = vec![
            QueryOutput {
                query_path: "$.queries[0].text".to_string(),
                query: "q0".to_string(),
                query_id: None,
                filter: None,
                metadata: None,
                task: None,
                completed: false,
                results: Vec::new(),
            },
            QueryOutput {
                query_path: "$.queries[1].text".to_string(),
                query: "q1".to_string(),
                query_id: None,
                filter: None,
                metadata: None,
                task: None,
                completed: false,
                results: Vec::new(),
            },
        ];
        let existing = vec![QueryOutput {
            query_path: "$.queries[0].text".to_string(),
            query: "q0".to_string(),
            query_id: None,
            filter: None,
            metadata: None,
            task: None,
            completed: true,
            results: vec![SearchOutput {
                id: "m0".to_string(),
                text: "hit".to_string(),
                metadata: serde_json::Value::Null,
                score: 0.9,
            }],
        }];

        let completed = resume_completed_indexes(&templates, &existing);

        assert_eq!(completed, vec![0]);
    }

    #[test]
    fn resume_completed_indexes_keeps_completed_empty_results() {
        let templates = vec![QueryOutput {
            query_path: "$.queries[0].text".to_string(),
            query: "q0".to_string(),
            query_id: Some("Q0".to_string()),
            filter: None,
            metadata: None,
            task: None,
            completed: false,
            results: Vec::new(),
        }];
        let existing = vec![QueryOutput {
            query_path: "$.queries[0].text".to_string(),
            query: "q0".to_string(),
            query_id: Some("Q0".to_string()),
            filter: None,
            metadata: None,
            task: None,
            completed: true,
            results: Vec::new(),
        }];

        assert_eq!(resume_completed_indexes(&templates, &existing), vec![0]);
    }

    #[test]
    fn build_runtime_does_not_require_graph_llm_api_key_when_only_graph_search_is_enabled() {
        let cli = Cli::try_parse_from([
            "memory-bench",
            "--embedding",
            "hash",
            "--graph",
            "--graph-llm-api-key-env",
            "RAM_A_TEST_MISSING_GRAPH_KEY_SEARCH_ONLY",
            "search",
            "--query",
            "Where?",
            "--output",
            "outputs/search.json",
        ])
        .expect("parse CLI");

        let _runtime = build_runtime(&cli).expect("runtime without graph build API key");
    }

    #[test]
    fn build_runtime_rejects_graph_search_without_graph_retrieval() {
        let cli = Cli::try_parse_from([
            "memory-bench",
            "--embedding",
            "hash",
            "--search-mode",
            "graph",
            "search",
            "--query",
            "Where?",
            "--output",
            "outputs/search.json",
        ])
        .expect("parse graph search CLI");

        let error = match build_runtime(&cli) {
            Ok(_) => panic!("graph search should require --graph"),
            Err(error) => error,
        };

        assert!(format!("{error}").contains("--search-mode graph requires --graph"));
    }

    #[test]
    fn build_runtime_requires_graph_llm_api_key_when_graph_build_is_enabled() {
        let cli = Cli::try_parse_from([
            "memory-bench",
            "--embedding",
            "hash",
            "--graph-build",
            "--graph-llm-api-key-env",
            "RAM_A_TEST_MISSING_GRAPH_KEY_BUILD",
            "add",
            "--dataset",
            "data/test.json",
        ])
        .expect("parse CLI");

        let error = match build_runtime(&cli) {
            Ok(_) => panic!("missing graph LLM API key should fail"),
            Err(error) => error,
        };

        assert!(format!("{error}")
            .contains("missing graph LLM API key env RAM_A_TEST_MISSING_GRAPH_KEY_BUILD"));
    }

    #[test]
    fn build_runtime_rejects_graph_build_with_jsonl_store() {
        let cli = Cli::try_parse_from([
            "memory-bench",
            "--embedding",
            "hash",
            "--store-backend",
            "jsonl",
            "--graph-build",
            "add",
            "--dataset",
            "data/test.json",
        ])
        .expect("parse CLI");

        let error = match build_runtime(&cli) {
            Ok(_) => panic!("jsonl graph build should fail"),
            Err(error) => error,
        };

        assert!(format!("{error}").contains("--graph-build requires --store-backend sqlite"));
    }

    #[tokio::test]
    async fn resume_add_builds_graph_for_existing_memory_records() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dataset = temp.path().join("dataset.json");
        std::fs::write(&dataset, r#"[{"text":"Alice lives in Shanghai."}]"#)
            .expect("write dataset");
        let db_path = temp.path().join("memory.sqlite");
        let store: Arc<dyn MemoryStore> = Arc::new(SqliteMemoryStore::new(&db_path));
        let text_fields = vec!["text".to_string()];
        let first_cli = Cli::try_parse_from([
            "memory-bench",
            "--embedding",
            "hash",
            "add",
            "--dataset",
            dataset.to_str().expect("dataset path"),
        ])
        .expect("parse first add CLI");
        let first_manager = MemoryManager::new(store.clone(), Arc::new(HashEmbedding::new(8)));

        run_add(
            first_manager,
            store.clone(),
            None,
            AddRunOptions {
                dataset: dataset.clone(),
                text_fields: &text_fields,
                batch_size: 1,
                resume: false,
                cli: &first_cli,
            },
        )
        .await
        .expect("baseline add");

        let repository = memory_core::sqlite::GraphRepository::open(&db_path);
        assert_eq!(repository.count_facts("path:$[0]").await.unwrap(), 0);

        let graph_pipeline = GraphBuildPipeline::new(
            repository.clone(),
            Arc::new(HashEmbedding::new(8)),
            Arc::new(BenchGraphExtractor),
            GraphTypeRegistry::new_default(),
        );
        let resume_cli = Cli::try_parse_from([
            "memory-bench",
            "--embedding",
            "hash",
            "--graph-build",
            "add",
            "--dataset",
            dataset.to_str().expect("dataset path"),
            "--resume",
        ])
        .expect("parse resume add CLI");
        let resume_manager = MemoryManager::new(store.clone(), Arc::new(HashEmbedding::new(8)));

        run_add(
            resume_manager,
            store,
            Some(Arc::new(graph_pipeline)),
            AddRunOptions {
                dataset,
                text_fields: &text_fields,
                batch_size: 1,
                resume: true,
                cli: &resume_cli,
            },
        )
        .await
        .expect("resume graph build");

        assert_eq!(repository.count_facts("path:$[0]").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn graph_build_concurrency_builds_every_record() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dataset = temp.path().join("dataset.json");
        std::fs::write(
            &dataset,
            r#"[{"text":"Alice lives in Shanghai."},{"text":"Alice lives in Shanghai."}]"#,
        )
        .expect("write dataset");
        let db_path = temp.path().join("memory.sqlite");
        let store: Arc<dyn MemoryStore> = Arc::new(SqliteMemoryStore::new(&db_path));
        let repository = memory_core::sqlite::GraphRepository::open(&db_path);
        let pipeline = Arc::new(GraphBuildPipeline::new(
            repository.clone(),
            Arc::new(HashEmbedding::new(8)),
            Arc::new(BenchGraphExtractor),
            GraphTypeRegistry::new_default(),
        ));
        let cli = Cli::try_parse_from([
            "memory-bench",
            "--embedding",
            "hash",
            "--graph-build",
            "--graph-build-concurrency",
            "2",
            "add",
            "--dataset",
            dataset.to_str().expect("dataset path"),
        ])
        .expect("parse graph build CLI");
        let manager = MemoryManager::new(store.clone(), Arc::new(HashEmbedding::new(8)));
        let text_fields = vec!["text".to_string()];

        run_add(
            manager,
            store,
            Some(pipeline),
            AddRunOptions {
                dataset,
                text_fields: &text_fields,
                batch_size: 1,
                resume: false,
                cli: &cli,
            },
        )
        .await
        .expect("concurrent graph build");

        assert_eq!(repository.count_facts("path:$[0]").await.unwrap(), 1);
        assert_eq!(repository.count_facts("path:$[1]").await.unwrap(), 1);
    }

    fn graph_test_cli() -> Cli {
        Cli::try_parse_from([
            "memory-bench",
            "--embedding",
            "hash",
            "search",
            "--query",
            "Where?",
            "--output",
            "outputs/search.json",
        ])
        .expect("parse graph test CLI")
    }

    fn graph_enabled_test_cli() -> Cli {
        Cli::try_parse_from([
            "memory-bench",
            "--embedding",
            "hash",
            "--graph",
            "search",
            "--query",
            "Where?",
            "--output",
            "outputs/search.json",
        ])
        .expect("parse graph search CLI")
    }
}
