// Event registry: maps event type names to their handler instances and specs.

use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

// Metadata describing one event type, used by the list_events meta-event and required-field validation.
pub struct EventSpec {
    pub description: String,
    // Required payload fields, e.g. ["session_id", "chunk_hashes"].
    pub required: Vec<String>,
    // Optional payload fields, e.g. ["assistant_text", "debug_context"].
    pub optional: Vec<String>,
}

pub enum EventResult {
    Ok(Value),
    Err(String),
}

// Shared context handed to every handler.
pub struct AppContext {
    pub manager: Arc<manager_core::KvCacheManager>,
    pub session_store: Arc<crate::session_store::SqliteSessionStore>,
}

#[async_trait]
pub trait EventHandler: Send + Sync {
    fn spec(&self) -> EventSpec;
    async fn handle(&self, payload: &Value, ctx: &AppContext) -> EventResult;
}

pub struct EventRegistry {
    handlers: HashMap<String, Arc<dyn EventHandler>>,
    specs: HashMap<String, EventSpec>,
}

impl EventRegistry {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
            specs: HashMap::new(),
        }
    }

    pub fn register(&mut self, type_name: &str, handler: Arc<dyn EventHandler>) {
        let spec = handler.spec();
        self.handlers.insert(type_name.to_string(), handler);
        self.specs.insert(type_name.to_string(), spec);
    }

    pub fn get_handler(&self, type_name: &str) -> Option<Arc<dyn EventHandler>> {
        self.handlers.get(type_name).cloned()
    }

    pub fn get_spec(&self, type_name: &str) -> Option<&EventSpec> {
        self.specs.get(type_name)
    }

    pub fn list_events(&self) -> Vec<(String, &EventSpec)> {
        self.specs
            .iter()
            .map(|(name, spec)| (name.clone(), spec))
            .collect()
    }
}
