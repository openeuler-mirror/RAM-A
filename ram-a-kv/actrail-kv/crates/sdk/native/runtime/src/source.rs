//! 保留分析来源保护信息，让独立于实例的导出复用已有输入和存储保护。
use crate::{Error, Result};
use actrail_kv_storage::StorageConfig;
use config_contract::{toml_edit::DocumentMut, Config};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceProtection {
    File { path: PathBuf },
    Storage { config_toml: String },
}
impl SourceProtection {
    pub fn storage(config: &StorageConfig) -> Result<Self> {
        let document = DocumentMut::from(config.try_to_toml_table().map_err(Error::config)?);
        Ok(Self::Storage {
            config_toml: document.to_string(),
        })
    }
    pub fn json(&self) -> Value {
        serde_json::to_value(self).expect("source metadata serializes")
    }
    pub fn from_response(bytes: &[u8]) -> Result<Option<Self>> {
        crate::request(bytes)?
            .get("source")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(Error::input)
    }
    pub fn validate_output(&self, output: &Path) -> Result<()> {
        match self {
            Self::Storage { config_toml } => {
                let document: DocumentMut = config_toml.parse().map_err(Error::input)?;
                StorageConfig::parse_toml_table(document.as_table())
                    .map_err(Error::input)?
                    .0
                    .validate_output_path(output)
                    .map_err(Error::storage)
            }
            Self::File { path } => {
                let overlap = if output.exists() {
                    same_file::is_same_file(path, output).map_err(Error::storage)?
                } else {
                    false
                };
                if overlap {
                    return Err(Error::storage("output overlaps with analysis input"));
                }
                Ok(())
            }
        }
    }
}
