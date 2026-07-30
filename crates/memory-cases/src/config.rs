use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{ArgGroup, Parser};

#[derive(Debug, Parser)]
#[command(about = "Case-oriented RAG service backed by memory-core")]
#[command(group(
    ArgGroup::new("mode")
        .required(true)
        .args(["api", "ingestor"])
))]
pub struct Cli {
    #[arg(long)]
    pub api: bool,

    #[arg(long)]
    pub ingestor: bool,

    #[arg(long, default_value = "127.0.0.1:8080")]
    pub bind: SocketAddr,

    #[arg(long = "rag-store", value_name = "PATH")]
    pub rag_store: Option<PathBuf>,

    #[arg(long = "memory-store", value_name = "PATH")]
    pub memory_store: Option<PathBuf>,

    #[arg(long, default_value_t = 256)]
    pub embedding_dimensions: usize,

    #[arg(long = "chunk-size", default_value_t = 512)]
    pub chunk_size: usize,

    #[arg(long, default_value_t = 1000)]
    pub poll_ms: u64,

    #[arg(long = "summary-llm-model")]
    pub summary_llm_model: Option<String>,

    #[arg(long = "summary-llm-api-key-env", default_value = "OPENAI_API_KEY")]
    pub summary_llm_api_key_env: String,

    #[arg(long = "summary-llm-base-url", default_value = "https://api.openai.com/v1")]
    pub summary_llm_base_url: String,

    #[arg(long = "summary-llm-timeout-ms", default_value_t = 30000)]
    pub summary_llm_timeout_ms: u64,
}

#[derive(Clone, Debug)]
pub struct StoragePaths {
    pub rag_store: PathBuf,
    pub memory_store: PathBuf,
}

impl Cli {
    pub fn storage_paths(&self) -> StoragePaths {
        let rag_default = PathBuf::from("data/memory-cases.sqlite");
        let memory_default = PathBuf::from("data/memory-cases-index.sqlite");

        let rag_store = self
            .rag_store
            .clone()
            .unwrap_or(rag_default);
        let memory_store = self
            .memory_store
            .clone()
            .unwrap_or(memory_default);

        StoragePaths {
            rag_store,
            memory_store,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_paths_default_to_split_files() {
        let cli = Cli::parse_from(["memory-cases", "--api"]);
        let paths = cli.storage_paths();

        assert_eq!(paths.rag_store, PathBuf::from("data/memory-cases.sqlite"));
        assert_eq!(
            paths.memory_store,
            PathBuf::from("data/memory-cases-index.sqlite")
        );
    }

    #[test]
    fn explicit_split_store_paths_override_defaults() {
        let cli = Cli::parse_from([
            "memory-cases",
            "--api",
            "--rag-store",
            "data/business.sqlite",
            "--memory-store",
            "data/index.sqlite",
        ]);
        let paths = cli.storage_paths();

        assert_eq!(paths.rag_store, PathBuf::from("data/business.sqlite"));
        assert_eq!(paths.memory_store, PathBuf::from("data/index.sqlite"));
    }

    #[test]
    fn store_argument_is_not_supported() {
        let result =
            Cli::try_parse_from(["memory-cases", "--api", "--store", "data/one-file.sqlite"]);

        assert!(result.is_err());
    }

    #[test]
    fn rebuild_index_argument_is_not_supported() {
        let result = Cli::try_parse_from(["memory-cases", "--rebuild-index"]);

        assert!(result.is_err());
    }
}
