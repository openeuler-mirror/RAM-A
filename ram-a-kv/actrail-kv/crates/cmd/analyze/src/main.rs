//! 本文件提供 actrail-kv-analyze 的离线 CLI 入口：JSON 配置文件 + 显式参数覆盖。

mod selection;

use std::path::PathBuf;

use actrail_kv_analyzer::config::AnalyzeConfig;
use actrail_kv_analyzer::run::{analyze_captures_with_selection, analyze_file_with_selection};
use actrail_kv_storage::{open_store, StorageConfig, StoreAccess};
use anyhow::{Context, Result};
use clap::Parser;

use selection::SelectionArgs;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Analyze captured LLM requests for structural KV cache disruption"
)]
struct Cli {
    #[arg(
        long,
        required_unless_present = "storage_config",
        conflicts_with = "storage_config"
    )]
    input: Option<PathBuf>,
    #[arg(long)]
    storage_config: Option<PathBuf>,
    #[command(flatten)]
    selection: SelectionArgs,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long, value_parser = parse_positive_usize)]
    top_k: Option<usize>,
    #[arg(long, value_parser = parse_positive_u64)]
    comparison_window_seconds: Option<u64>,
}

fn parse_positive_usize(value: &str) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|error| format!("invalid positive integer: {error}"))?;
    if parsed == 0 {
        return Err("value must be greater than zero".to_owned());
    }
    Ok(parsed)
}

fn parse_positive_u64(value: &str) -> Result<u64, String> {
    let parsed = value
        .parse::<u64>()
        .map_err(|error| format!("invalid positive integer: {error}"))?;
    if parsed == 0 {
        return Err("value must be greater than zero".to_owned());
    }
    Ok(parsed)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut config = match &cli.config {
        Some(path) => {
            let raw = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read config {}", path.display()))?;
            AnalyzeConfig::from_json_str(&raw)
                .with_context(|| format!("invalid config {}", path.display()))?
        }
        None => AnalyzeConfig::default(),
    };
    if let Some(top_k) = cli.top_k {
        config.top_k = top_k;
    }
    if let Some(window) = cli.comparison_window_seconds {
        config.comparison_window_seconds = window;
    }
    let options = config.into_options()?;
    let (selection, scan_filter) = cli.selection.into_filters()?;
    if let Some(path) = cli.storage_config {
        let storage = StorageConfig::from_file(&path)
            .with_context(|| format!("failed to load storage config {}", path.display()))?;
        storage.validate_output(&path, &cli.output)?;
        let store = open_store(storage, StoreAccess::ReadOnly).await?;
        analyze_captures_with_selection(
            store.scan(scan_filter).await?,
            &cli.output,
            options,
            &selection,
        )
        .await?;
    } else {
        let input = cli.input.context("missing analysis input")?;
        analyze_file_with_selection(&input, &cli.output, options, &selection)?;
    }
    Ok(())
}
