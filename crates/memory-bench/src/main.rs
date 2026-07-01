use std::collections::HashSet;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use memory_core::{
    AddMemoryRequest, EmbeddingProvider, FileMemoryStore, HashEmbedding, LongTermMemory,
    MemoryManager, MemoryStore, OpenRouterEmbedding, RetrievalConfig, SearchMemoryRequest,
    SearchMode, SqliteMemoryStore,
};
use serde::Serialize;

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
    Hybrid,
}

impl From<SearchModeKind> for SearchMode {
    fn from(value: SearchModeKind) -> Self {
        match value {
            SearchModeKind::Dense => SearchMode::Dense,
            SearchModeKind::Bm25 => SearchMode::Bm25,
            SearchModeKind::Hybrid => SearchMode::Hybrid,
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
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let runtime = build_runtime(&cli)?;

    match cli.command {
        Command::Add {
            dataset,
            text_fields,
            resume,
        } => {
            run_add(
                runtime.manager,
                runtime.store,
                dataset,
                &text_fields,
                cli.batch_size,
                resume,
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
        } => {
            let options = SearchRunOptions {
                output,
                top_k,
                query_fields: &query_fields,
                filter,
                batch_size: cli.batch_size,
            };
            run_search(runtime.manager, dataset, query, options).await
        }
    }
}

struct BenchRuntime {
    manager: MemoryManager,
    store: Arc<dyn MemoryStore>,
}

fn build_runtime(cli: &Cli) -> Result<BenchRuntime> {
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
    let manager = MemoryManager::with_retrieval_config(
        store.clone(),
        embedder,
        RetrievalConfig {
            mode: cli.search_mode.into(),
            embedding_weight: cli.embedding_weight,
            bm25_weight: cli.bm25_weight,
            candidate_k: cli.candidate_k,
        },
    );
    Ok(BenchRuntime { manager, store })
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
    dataset: PathBuf,
    text_fields: &[String],
    batch_size: usize,
    resume: bool,
) -> Result<()> {
    let json = load_json(&dataset)?;
    if is_prepared_schema_v1(&json) && json.get("memories").is_some() {
        return run_add_prepared_memories(manager, store, dataset, &json, batch_size, resume).await;
    }

    let mut texts = Vec::new();
    collect_field_texts(&json, "$", text_fields, &mut texts);

    if texts.is_empty() {
        bail!(
            "no memory texts found in {} using fields {:?}",
            dataset.display(),
            text_fields
        );
    }

    let existing_ids = existing_ids_for_resume(&store, resume).await?;
    let mut summary = AddSummary {
        total: texts.len(),
        ..Default::default()
    };
    let mut requests = Vec::with_capacity(texts.len());
    for (index, item) in texts.iter().enumerate() {
        let id = format!("{}:{}", item.path, index);
        if should_skip_existing(&existing_ids, &id) {
            summary.skipped_existing += 1;
            continue;
        }

        let mut metadata = serde_json::json!({
            "dataset": dataset.display().to_string(),
            "path": item.path,
            "field": item.field,
        });
        merge_metadata(&mut metadata, &item.metadata);

        requests.push(AddMemoryRequest {
            id: Some(id),
            text: item.text.clone(),
            metadata,
        });
    }

    if !requests.is_empty() {
        let mut progress = ProgressReporter::new("Adding memories", requests.len());
        manager
            .add_many_with_batch_size_and_progress(requests, batch_size, |count| {
                progress.inc(count);
            })
            .await
            .with_context(|| format!("failed to add memories from {}", dataset.display()))
            .inspect_err(|_| {
                summary.failed += 1;
                print_add_summary(&summary);
            })?;
        progress.finish();
    }
    summary.added = summary.total - summary.skipped_existing;

    println!("added {} memories from {}", texts.len(), dataset.display());
    print_add_summary(&summary);
    Ok(())
}

async fn run_add_prepared_memories(
    manager: MemoryManager,
    store: Arc<dyn MemoryStore>,
    dataset: PathBuf,
    json: &serde_json::Value,
    batch_size: usize,
    resume: bool,
) -> Result<()> {
    let memories = json
        .get("memories")
        .and_then(|value| value.as_array())
        .context("prepared schema v1 field `memories` must be a JSON array")?;

    if memories.is_empty() {
        bail!("prepared schema v1 field `memories` must contain at least one memory");
    }

    let existing_ids = existing_ids_for_resume(&store, resume).await?;
    let mut summary = AddSummary {
        total: memories.len(),
        ..Default::default()
    };
    let mut requests = Vec::with_capacity(memories.len());
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
        if should_skip_existing(&existing_ids, &id) {
            summary.skipped_existing += 1;
            continue;
        }

        let mut metadata = match object.get("metadata") {
            Some(value) => {
                if !value.is_object() {
                    bail!("memories[{index}].metadata must be a JSON object when present");
                }
                value.clone()
            }
            None => serde_json::Value::Object(Default::default()),
        };
        insert_default_metadata(
            &mut metadata,
            "dataset",
            serde_json::Value::String(dataset.display().to_string()),
        );
        insert_default_metadata(
            &mut metadata,
            "path",
            serde_json::Value::String(format!("$.memories[{index}].text")),
        );
        insert_default_metadata(
            &mut metadata,
            "field",
            serde_json::Value::String("text".to_string()),
        );

        requests.push(AddMemoryRequest {
            id: Some(id),
            text: text.to_string(),
            metadata,
        });
    }

    if !requests.is_empty() {
        let mut progress = ProgressReporter::new("Adding prepared memories", requests.len());
        manager
            .add_many_with_batch_size_and_progress(requests, batch_size, |count| {
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
    summary.added = summary.total - summary.skipped_existing;

    println!(
        "added {} prepared memories from {}",
        memories.len(),
        dataset.display()
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
}

async fn run_search(
    manager: MemoryManager,
    dataset: Option<PathBuf>,
    query: Option<String>,
    options: SearchRunOptions<'_>,
) -> Result<()> {
    let cli_filter = parse_filter(options.filter)?;
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
            return run_search_prepared_queries(
                manager,
                &json,
                options.output,
                options.top_k,
                cli_filter,
                options.batch_size,
            )
            .await;
        }
        collect_field_texts(&json, "$", options.query_fields, &mut queries);
    }
    if queries.is_empty() {
        bail!("no queries provided; pass --query or --dataset with query fields");
    }

    let mut outputs = Vec::new();
    if queries.len() == 1 {
        let item = queries.into_iter().next().unwrap();
        let results = manager
            .search(SearchMemoryRequest {
                query: item.text.clone(),
                top_k: options.top_k,
                filter: cli_filter.clone(),
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
            results: results
                .into_iter()
                .map(|result| SearchOutput {
                    id: result.record.id,
                    text: result.record.text,
                    metadata: result.record.metadata,
                    score: result.score,
                })
                .collect(),
        });
    } else if !queries.is_empty() {
        let search_requests: Vec<SearchMemoryRequest> = queries
            .iter()
            .map(|item| SearchMemoryRequest {
                query: item.text.clone(),
                top_k: options.top_k,
                filter: cli_filter.clone(),
            })
            .collect();
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
                results: results
                    .into_iter()
                    .map(|result| SearchOutput {
                        id: result.record.id,
                        text: result.record.text,
                        metadata: result.record.metadata,
                        score: result.score,
                    })
                    .collect(),
            })
            .collect();
    }

    if let Some(parent) = options.output.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&options.output, serde_json::to_vec_pretty(&outputs)?).await?;
    println!("wrote search results to {}", options.output.display());
    Ok(())
}

async fn run_search_prepared_queries(
    manager: MemoryManager,
    json: &serde_json::Value,
    output: PathBuf,
    top_k: usize,
    cli_filter: Option<serde_json::Value>,
    batch_size: usize,
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
            top_k,
            filter: effective_filter.clone(),
        });
        output_templates.push(QueryOutput {
            query_path: format!("$.queries[{index}].text"),
            query: query_text.to_string(),
            query_id,
            filter: effective_filter,
            metadata,
            task,
            results: Vec::new(),
        });
    }

    let mut progress = ProgressReporter::new("Searching prepared queries", search_requests.len());
    let search_results = manager
        .search_many_with_batch_size_and_progress(search_requests, batch_size, |count| {
            progress.inc(count);
        })
        .await
        .with_context(|| "failed to search prepared queries in batch")?;
    progress.finish();
    let outputs = output_templates
        .into_iter()
        .zip(search_results)
        .map(|(mut output, results)| {
            output.results = results
                .into_iter()
                .map(|result| SearchOutput {
                    id: result.record.id,
                    text: result.record.text,
                    metadata: result.record.metadata,
                    score: result.score,
                })
                .collect();
            output
        })
        .collect::<Vec<_>>();

    if let Some(parent) = output.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&output, serde_json::to_vec_pretty(&outputs)?).await?;
    println!("wrote search results to {}", output.display());
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

#[derive(Serialize)]
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
    results: Vec<SearchOutput>,
}

#[derive(Serialize)]
struct SearchOutput {
    id: String,
    text: String,
    metadata: serde_json::Value,
    score: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
