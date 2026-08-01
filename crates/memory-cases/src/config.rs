use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{ArgGroup, Parser};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmbeddingProviderKind {
    Hash,
    OpenAiCompatible,
}

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

    #[arg(long = "api-token-env", default_value = "MEMORY_CASES_API_TOKEN")]
    pub api_token_env: String,

    #[arg(long = "rag-store", value_name = "PATH")]
    pub rag_store: Option<PathBuf>,

    #[arg(long = "memory-store", value_name = "PATH")]
    pub memory_store: Option<PathBuf>,

    #[arg(long, default_value_t = 256)]
    pub embedding_dimensions: usize,

    #[arg(
        long = "embedding-provider",
        default_value = "hash",
        value_parser = parse_embedding_provider
    )]
    pub embedding_provider: EmbeddingProviderKind,

    #[arg(long = "embedding-api-key-env", default_value = "OPENAI_API_KEY")]
    pub embedding_api_key_env: String,

    #[arg(
        long = "embedding-base-url",
        default_value = "https://api.openai.com/v1"
    )]
    pub embedding_base_url: String,

    #[arg(long = "embedding-model", default_value = "text-embedding-3-small")]
    pub embedding_model: String,

    #[arg(long = "chunk-size", default_value_t = 512)]
    pub chunk_size: usize,

    #[arg(long, default_value_t = 1000)]
    pub poll_ms: u64,

    #[arg(long = "summary-llm-model")]
    pub summary_llm_model: Option<String>,

    #[arg(long = "summary-llm-api-key-env", default_value = "OPENAI_API_KEY")]
    pub summary_llm_api_key_env: String,

    #[arg(
        long = "summary-llm-base-url",
        default_value = "https://api.openai.com/v1"
    )]
    pub summary_llm_base_url: String,

    #[arg(long = "summary-llm-timeout-ms", default_value_t = 30000)]
    pub summary_llm_timeout_ms: u64,
}

pub fn parse_embedding_provider(value: &str) -> Result<EmbeddingProviderKind, String> {
    match value {
        "hash" => Ok(EmbeddingProviderKind::Hash),
        "openai_compatible" | "open_router" => Ok(EmbeddingProviderKind::OpenAiCompatible),
        other => Err(format!(
            "unsupported embedding provider `{other}`; expected `hash` or `openai_compatible`"
        )),
    }
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

        let rag_store = self.rag_store.clone().unwrap_or(rag_default);
        let memory_store = self.memory_store.clone().unwrap_or(memory_default);

        StoragePaths {
            rag_store,
            memory_store,
        }
    }

    pub fn resolve_api_token(&self) -> anyhow::Result<String> {
        let env_name = self.api_token_env.trim();
        anyhow::ensure!(
            !env_name.is_empty() && env_name == self.api_token_env,
            "API token environment name must be canonical and non-empty"
        );
        let token = std::env::var_os(env_name)
            .ok_or_else(|| {
                anyhow::anyhow!("API token environment variable `{env_name}` is unavailable")
            })?
            .into_string()
            .map_err(|_| {
                anyhow::anyhow!("API token environment variable `{env_name}` is not valid Unicode")
            })?;
        anyhow::ensure!(
            !token.trim().is_empty() && token.trim() == token,
            "API token environment variable `{env_name}` must be canonical and non-empty"
        );
        Ok(token)
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

    #[test]
    fn embedding_provider_defaults_to_hash() {
        let cli = Cli::parse_from(["memory-cases", "--api"]);

        assert_eq!(cli.embedding_provider, EmbeddingProviderKind::Hash);
    }

    #[test]
    fn embedding_provider_accepts_openai_compatible_and_open_router_alias() {
        let cli = Cli::parse_from([
            "memory-cases",
            "--api",
            "--embedding-provider",
            "openai_compatible",
        ]);
        assert_eq!(
            cli.embedding_provider,
            EmbeddingProviderKind::OpenAiCompatible
        );

        let alias = Cli::parse_from([
            "memory-cases",
            "--api",
            "--embedding-provider",
            "open_router",
        ]);
        assert_eq!(
            alias.embedding_provider,
            EmbeddingProviderKind::OpenAiCompatible
        );
    }

    #[test]
    fn api_token_is_loaded_from_the_named_environment_variable() {
        const ENV_NAME: &str = "RAM_A_MEMORY_CASES_CONFIG_TEST_TOKEN";
        std::env::set_var(ENV_NAME, "internal-secret");
        let cli = Cli::parse_from(["memory-cases", "--api", "--api-token-env", ENV_NAME]);

        let token = cli.resolve_api_token().unwrap();

        std::env::remove_var(ENV_NAME);
        assert_eq!(token, "internal-secret");
    }

    #[test]
    fn api_token_configuration_rejects_missing_or_noncanonical_values() {
        const ENV_NAME: &str = "RAM_A_MEMORY_CASES_CONFIG_TEST_MISSING_TOKEN";
        std::env::remove_var(ENV_NAME);
        let missing = Cli::parse_from(["memory-cases", "--api", "--api-token-env", ENV_NAME]);
        assert!(missing.resolve_api_token().is_err());

        let noncanonical =
            Cli::parse_from(["memory-cases", "--api", "--api-token-env", " PADDED_ENV "]);
        assert!(noncanonical.resolve_api_token().is_err());

        std::env::set_var(ENV_NAME, " ");
        assert!(missing.resolve_api_token().is_err());
        std::env::remove_var(ENV_NAME);
    }
}
