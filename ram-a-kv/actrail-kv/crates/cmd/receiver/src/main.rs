//! 本二进制加载 receiver 配置、解析参数覆盖、绑定监听并处理优雅关闭。

use std::{net::SocketAddr, path::PathBuf};

use actrail_kv_receiver::{router_with_storage, ReceiverConfig};
use actrail_kv_storage::{open_legacy_ndjson, open_store, StorageConfig, StoreAccess};
use anyhow::{Context, Result};
use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "actrail-kv-receiver")]
#[command(about = "Receive and store complete model requests")]
struct Arguments {
    #[arg(long)]
    listen: Option<SocketAddr>,
    #[arg(
        long,
        required_unless_present = "storage_config",
        conflicts_with = "storage_config"
    )]
    output: Option<PathBuf>,
    #[arg(long)]
    storage_config: Option<PathBuf>,
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long, value_parser = parse_positive_usize)]
    max_payload_bytes: Option<usize>,
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

#[tokio::main]
async fn main() -> Result<()> {
    let arguments = Arguments::parse();
    let mut config = match &arguments.config {
        Some(path) => {
            let raw = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read config {}", path.display()))?;
            ReceiverConfig::from_json_str(&raw)
                .with_context(|| format!("invalid config {}", path.display()))?
        }
        None => ReceiverConfig::default(),
    };
    if let Some(listen) = arguments.listen {
        config.listen = listen.to_string();
    }
    if let Some(max_payload_bytes) = arguments.max_payload_bytes {
        config.max_payload_bytes = max_payload_bytes;
    }
    config.validate()?;
    let listen: SocketAddr = config
        .listen
        .parse()
        .with_context(|| format!("invalid listen address {}", config.listen))?;
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("failed to bind receiver to {}", listen))?;
    let store = if let Some(path) = arguments.storage_config {
        let storage = StorageConfig::from_file(&path)
            .with_context(|| format!("failed to load storage config {}", path.display()))?;
        open_store(storage, StoreAccess::ReadWrite).await?
    } else {
        let output = arguments.output.context("missing receiver output")?;
        open_legacy_ndjson(output.clone())
            .await
            .with_context(|| format!("failed to open receiver output {}", output.display()))?
    };
    eprintln!("listening on {}", listener.local_addr()?);

    axum::serve(
        listener,
        router_with_storage(store, config.max_payload_bytes),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("receiver server failed")
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

#[cfg(test)]
#[path = "../../../../tests/unit/cmd/receiver/main.rs"]
mod tests;
