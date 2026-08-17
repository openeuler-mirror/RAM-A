use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KvCacheMap(pub Vec<String>);

impl KvCacheMap {
    pub fn chunk_hashes(&self) -> Vec<String> {
        self.0.clone()
    }

    pub fn replace(&mut self, new_hashes: &[String]) {
        self.0 = new_hashes.to_vec();
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}
