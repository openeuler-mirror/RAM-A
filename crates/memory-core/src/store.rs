use std::any::Any;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::fs;

use crate::{MemoryRecord, MemoryResult};

#[async_trait]
pub trait MemoryStore: Send + Sync {
    fn as_any(&self) -> &dyn Any;
    async fn add_record(&self, record: &MemoryRecord) -> MemoryResult<()>;
    async fn list_records(&self) -> MemoryResult<Vec<MemoryRecord>>;
    async fn replace_all(&self, records: &[MemoryRecord]) -> MemoryResult<()>;
}

pub struct FileMemoryStore {
    path: PathBuf,
}

impl FileMemoryStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[async_trait]
impl MemoryStore for FileMemoryStore {
    fn as_any(&self) -> &dyn Any {
        self
    }

    async fn add_record(&self, record: &MemoryRecord) -> MemoryResult<()> {
        let mut records = self.list_records().await?;
        records.retain(|existing| existing.id != record.id);
        records.push(record.clone());
        self.replace_all(&records).await
    }

    async fn list_records(&self) -> MemoryResult<Vec<MemoryRecord>> {
        match fs::read_to_string(&self.path).await {
            Ok(content) => {
                let mut records = Vec::new();
                for line in content.lines().filter(|line| !line.trim().is_empty()) {
                    records.push(serde_json::from_str(line)?);
                }
                Ok(records)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error.into()),
        }
    }

    async fn replace_all(&self, records: &[MemoryRecord]) -> MemoryResult<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let mut content = String::new();
        for record in records {
            content.push_str(&serde_json::to_string(record)?);
            content.push('\n');
        }
        fs::write(&self.path, content).await?;
        Ok(())
    }
}
