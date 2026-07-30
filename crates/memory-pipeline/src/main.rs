use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{bail, Context};
use clap::Parser;
use memory_pipeline::cache::JsonCache;
use memory_pipeline::client::OpenAiCompatibleClient;
use memory_pipeline::episode::EpisodeConfig;
use memory_pipeline::extraction::{LlmMemoryExtractor, MemoryExtractor, StaticMemoryExtractor};
use memory_pipeline::grounding::{
    GroundingVerifier, LlmGroundingVerifier, StaticGroundingVerifier,
};
use memory_pipeline::pipeline::{
    run_memory_pipeline, write_pipeline_artifacts, write_prepared_output, PipelineConfig,
};
use memory_pipeline::validation::ValidationConfig;
use memory_pipeline::window::WindowConfig;
use serde_json::Value;

#[derive(Parser)]
#[command(about = "Convert benchmark-prepared messages into grounded atomic memories")]
struct Args {
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    artifacts_dir: PathBuf,
    #[arg(long)]
    extractor_responses: Option<PathBuf>,
    #[arg(long)]
    grounding_responses: Option<PathBuf>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    verifier_model: Option<String>,
    #[arg(long)]
    api_key_env: Option<String>,
    #[arg(long, default_value = "https://openrouter.ai/api/v1")]
    base_url: String,
    #[arg(long, default_value_t = 120)]
    timeout_seconds: u64,
    #[arg(long, default_value_t = 8)]
    max_retries: usize,
    #[arg(long)]
    max_time_gap_minutes: Option<i64>,
    #[arg(long = "episode-boundary-field")]
    episode_boundary_fields: Vec<String>,
    #[arg(long, default_value_t = 320)]
    max_candidate_tokens: usize,
    #[arg(long, default_value_t = 640)]
    max_window_tokens: usize,
    #[arg(long, default_value_t = 2)]
    context_before_messages: usize,
    #[arg(long, default_value_t = 0)]
    context_after_messages: usize,
    #[arg(long)]
    cache_dir: Option<PathBuf>,
    #[arg(long, default_value = "cache_v1")]
    cache_version: String,
    #[arg(long, default_value_t = false)]
    fail_fast: bool,
    #[arg(long, default_value_t = false, conflicts_with = "fail_fast")]
    no_fail_fast: bool,
    #[arg(long, default_value_t = 500)]
    max_memory_chars: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let prepared = read_object(&args.input)?;
    let (extractor, verifier): (Box<dyn MemoryExtractor>, Box<dyn GroundingVerifier>) =
        match (&args.extractor_responses, &args.grounding_responses) {
            (Some(extraction), Some(grounding)) => {
                if args.model.is_some()
                    || args.verifier_model.is_some()
                    || args.api_key_env.is_some()
                {
                    bail!("fixture mode cannot be combined with live model arguments");
                }
                (
                    Box::new(StaticMemoryExtractor::new(read_map(extraction)?)),
                    Box::new(StaticGroundingVerifier::new(read_map(grounding)?)),
                )
            }
            (None, None) => {
                let model = args
                    .model
                    .as_deref()
                    .context("live mode requires --model")?;
                let verifier_model = args
                    .verifier_model
                    .as_deref()
                    .context("live mode requires --verifier-model")?;
                let key_env = args
                    .api_key_env
                    .as_deref()
                    .context("live mode requires --api-key-env")?;
                let client = OpenAiCompatibleClient::from_env(
                    key_env,
                    &args.base_url,
                    args.timeout_seconds,
                    args.max_retries,
                )?;
                (
                    Box::new(LlmMemoryExtractor::new(client.clone(), model)),
                    Box::new(LlmGroundingVerifier::new(client, verifier_model)),
                )
            }
            _ => {
                bail!("fixture mode requires both --extractor-responses and --grounding-responses")
            }
        };
    let config = PipelineConfig {
        episode: EpisodeConfig {
            max_time_gap_minutes: args.max_time_gap_minutes,
            metadata_boundary_fields: args.episode_boundary_fields,
            ..EpisodeConfig::default()
        },
        window: WindowConfig {
            max_candidate_tokens: args.max_candidate_tokens,
            max_window_tokens: args.max_window_tokens,
            context_before_messages: args.context_before_messages,
            context_after_messages: args.context_after_messages,
            ..WindowConfig::default()
        },
        validation: ValidationConfig {
            max_memory_chars: args.max_memory_chars,
        },
        fail_fast: resolve_fail_fast(args.fail_fast, args.no_fail_fast),
        ..PipelineConfig::default()
    };
    let cache = args
        .cache_dir
        .map(|root| JsonCache::new(root, args.cache_version));
    let run = run_memory_pipeline(
        &prepared,
        &config,
        extractor.as_ref(),
        verifier.as_ref(),
        cache.as_ref(),
    )
    .await?;
    write_pipeline_artifacts(&run, &args.artifacts_dir)?;
    write_prepared_output(&args.output, &run.prepared)?;
    Ok(())
}

fn resolve_fail_fast(fail_fast: bool, no_fail_fast: bool) -> bool {
    match (fail_fast, no_fail_fast) {
        (true, true) => unreachable!("clap rejects conflicting fail-fast flags"),
        (_, true) => false,
        _ => true,
    }
}

fn read_object(path: &PathBuf) -> anyhow::Result<Value> {
    let value: Value = serde_json::from_slice(
        &std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?,
    )?;
    if !value.is_object() {
        bail!("{} must contain one JSON object", path.display());
    }
    Ok(value)
}

fn read_map(path: &PathBuf) -> anyhow::Result<HashMap<String, Value>> {
    let value = read_object(path)?;
    Ok(value
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::resolve_fail_fast;

    #[test]
    fn fail_fast_defaults_to_enabled_and_allows_explicit_best_effort() {
        assert!(resolve_fail_fast(false, false));
        assert!(resolve_fail_fast(true, false));
        assert!(!resolve_fail_fast(false, true));
    }
}
