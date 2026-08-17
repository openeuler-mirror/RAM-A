use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
pub struct SdkConfig {
    pub daemon_url: String,
    #[serde(default)]
    pub auth_token: String,
}

pub fn default_config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("ram-a-kv")
        .join("config.toml")
}

pub fn load_config(path: &Path) -> Option<SdkConfig> {
    let content = std::fs::read_to_string(path).ok()?;
    toml::from_str(&content).ok()
}
