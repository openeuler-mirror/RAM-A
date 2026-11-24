//! 本命令通过统一存储接口将匹配的采集记录导出为 NDJSON。

use std::{
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use actrail_kv_storage::{open_store, CaptureFilter, StorageConfig, StoreAccess};
use anyhow::{Context, Result};
use clap::Parser;
use futures_util::TryStreamExt;

#[derive(Debug, Parser)]
#[command(version, about = "Export captured model requests as NDJSON")]
struct Cli {
    #[arg(long)]
    storage_config: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    session_key: Option<String>,
    #[arg(long)]
    from: Option<String>,
    #[arg(long)]
    until: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let filter = CaptureFilter::parse(cli.session_key, cli.from.as_deref(), cli.until.as_deref())?;
    let config = StorageConfig::from_file(&cli.storage_config).with_context(|| {
        format!(
            "failed to load storage config {}",
            cli.storage_config.display()
        )
    })?;
    config.validate_output(&cli.storage_config, &cli.output)?;
    let store = open_store(config, StoreAccess::ReadOnly).await?;
    let mut rows = store.scan(filter).await?;

    let output_directory = cli
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty());
    let mut temporary =
        tempfile::NamedTempFile::new_in(output_directory.unwrap_or_else(|| Path::new(".")))?;
    {
        let mut output = BufWriter::new(temporary.as_file_mut());
        while let Some(record) = rows.try_next().await? {
            serde_json::to_writer(&mut output, &record)?;
            output.write_all(b"\n")?;
        }
        output.flush()?;
    }
    temporary.as_file().sync_all()?;
    temporary.persist(&cli.output)?;
    Ok(())
}
