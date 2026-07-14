use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GraphPredicate {
    pub name: String,
    pub description: String,
    pub temporal_kind: Option<String>,
    pub cardinality: Option<String>,
    pub overlap_allowed: Option<bool>,
    pub symmetric: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GraphTypeRegistry {
    pub version: String,
    pub entity_types: Vec<String>,
    pub predicates: Vec<GraphPredicate>,
}

impl GraphTypeRegistry {
    pub fn new_default() -> Self {
        Self {
            version: "graph-type-registry-v1".to_string(),
            entity_types: vec![
                "PERSON".to_string(),
                "LOCATION".to_string(),
                "ORGANIZATION".to_string(),
                "PROJECT".to_string(),
                "CONCEPT".to_string(),
            ],
            predicates: vec![GraphPredicate {
                name: "LIVES_IN".to_string(),
                description: "A person currently or historically lives in a location.".to_string(),
                temporal_kind: Some("state".to_string()),
                cardinality: Some("single".to_string()),
                overlap_allowed: Some(false),
                symmetric: false,
            }],
        }
    }

    pub fn predicate(&self, name: &str) -> Option<&GraphPredicate> {
        self.predicates
            .iter()
            .find(|predicate| predicate.name == name)
    }
}

impl Default for GraphTypeRegistry {
    fn default() -> Self {
        Self::new_default()
    }
}
