use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{json, Value};

use crate::canonical::stable_hash;
use crate::error::{PipelineError, Result};

pub struct JsonCache {
    root: PathBuf,
    pub version: String,
}

impl JsonCache {
    pub fn new(root: impl Into<PathBuf>, version: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            version: version.into(),
        }
    }

    pub fn get(&self, namespace: &str, key_parts: &[Value]) -> Result<Option<Value>> {
        let path = self.path(namespace, key_parts)?;
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)?;
        serde_json::from_str(&content).map(Some).map_err(|_| {
            PipelineError::Protocol(format!("corrupt cache entry: {}", path.display()))
        })
    }

    pub fn put(&self, namespace: &str, key_parts: &[Value], value: &Value) -> Result<PathBuf> {
        let path = self.path(namespace, key_parts)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, crate::canonical::canonical_json(value))?;
        std::fs::rename(&temporary, &path)?;
        Ok(path)
    }

    fn path(&self, namespace: &str, key_parts: &[Value]) -> Result<PathBuf> {
        let safe = Regex::new(r"^[A-Za-z0-9_.-]+$").expect("cache namespace regex is valid");
        if !safe.is_match(namespace) {
            return Err(PipelineError::InvalidInput(format!(
                "unsafe cache namespace: {namespace:?}"
            )));
        }
        let digest = stable_hash(&[json!(self.version), Value::Array(key_parts.to_vec())]);
        Ok(self.root.join(namespace).join(format!("{digest}.json")))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}
