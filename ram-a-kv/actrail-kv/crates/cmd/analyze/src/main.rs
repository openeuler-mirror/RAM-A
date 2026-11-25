//! 本文件提供 actrail-kv-analyze 的离线 CLI 入口：TOML 配置文件 + 显式参数覆盖。

mod selection;

use std::path::PathBuf;

use actrail_kv_analyzer::config::settings::AnalyzerSettings;
use actrail_kv_analyzer::run::{analyze_captures_with_selection, analyze_file_with_selection};
use actrail_kv_receiver::config::settings::ReceiverSettings;
use actrail_kv_settings::ConfigFile;
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
    #[arg(long, conflicts_with = "storage_config")]
    input: Option<PathBuf>,
    #[arg(long)]
    storage_config: Option<PathBuf>,
    #[command(flatten)]
    selection: SelectionArgs,
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long, value_parser = parse_positive_usize)]
    top_k: Option<usize>,
    #[arg(long, value_parser = parse_positive_u64)]
    comparison_window_seconds: Option<u64>,
}

enum CaptureInput {
    File(PathBuf),
    Store {
        config: StorageConfig,
        override_path: Option<PathBuf>,
    },
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
    let mut settings = ConfigFile::open(cli.config.as_deref())?;
    let file: AnalyzerSettings = settings.read(&["analyzer"])?;
    let mut config = file.options;
    if let Some(top_k) = cli.top_k {
        config.top_k = top_k;
    }
    if let Some(window) = cli.comparison_window_seconds {
        config.projection.comparison_window_seconds = window;
    }
    config.validate()?;
    let options = config;
    let (selection, scan_filter) = cli.selection.into_filters(file.selection)?;
    let output = cli.output.unwrap_or_else(|| settings.resolve(file.output));
    settings.check_output(&output)?;
    let input = if let Some(path) = cli.storage_config {
        let config = StorageConfig::from_file(&path)
            .with_context(|| format!("failed to load storage config {}", path.display()))?;
        CaptureInput::Store {
            config,
            override_path: Some(path),
        }
    } else {
        let input = cli
            .input
            .or_else(|| file.input.map(|path| settings.resolve(path)));
        let input = match input {
            Some(input) => Some(input),
            None => settings
                .read::<ReceiverSettings>(&["receiver"])?
                .output
                .map(|path| settings.resolve(path)),
        };
        match input {
            Some(input) => CaptureInput::File(input),
            None => {
                let storage: StorageConfig = settings.read(&["storage"])?;
                let config = storage.resolve(settings.directory())?;
                CaptureInput::Store {
                    config,
                    override_path: None,
                }
            }
        }
    };
    if let CaptureInput::Store {
        config,
        override_path,
    } = &input
    {
        config.validate_output(override_path.as_deref().unwrap_or(settings.path()), &output)?;
    }
    settings.finish()?;
    match input {
        CaptureInput::File(input) => {
            analyze_file_with_selection(&input, &output, options, &selection)?;
        }
        CaptureInput::Store { config, .. } => {
            let store = open_store(config, StoreAccess::ReadOnly).await?;
            analyze_captures_with_selection(
                store.scan(scan_filter).await?,
                &output,
                options,
                &selection,
            )
            .await?;
        }
    }
    Ok(())
}
