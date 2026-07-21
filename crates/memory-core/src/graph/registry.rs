use serde::{Deserialize, Serialize};

pub const GRAPH_FALLBACK_PREDICATE: &str = "RELATED_TO";

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
            version: "graph-type-registry-v2".to_string(),
            entity_types: vec![
                "PERSON".to_string(),
                "LOCATION".to_string(),
                "ORGANIZATION".to_string(),
                "PROJECT".to_string(),
                "EVENT".to_string(),
                "ACTIVITY".to_string(),
                "OBJECT".to_string(),
                "PREFERENCE".to_string(),
                "TIME".to_string(),
                "GROUP".to_string(),
                "CONCEPT".to_string(),
            ],
            predicates: vec![
                predicate(
                    "LIVES_IN",
                    "A person currently or historically lives in a location.",
                    Some("state"),
                    Some("single"),
                    Some(false),
                    false,
                ),
                predicate(
                    "WORKS_AT",
                    "A person works at or is professionally affiliated with an organization.",
                    Some("state"),
                    Some("many"),
                    Some(true),
                    false,
                ),
                predicate(
                    "STUDIES_AT",
                    "A person studies at or attends an educational organization.",
                    Some("state"),
                    Some("many"),
                    Some(true),
                    false,
                ),
                predicate(
                    "FAMILY_OF",
                    "Two people have a family relationship.",
                    Some("state"),
                    Some("many"),
                    Some(true),
                    true,
                ),
                predicate(
                    "FRIEND_OF",
                    "Two people have a friendship or close social relationship.",
                    Some("state"),
                    Some("many"),
                    Some(true),
                    true,
                ),
                predicate(
                    "LIKES",
                    "A person likes, prefers, or enjoys an entity, activity, topic, or object.",
                    Some("state"),
                    Some("many"),
                    Some(true),
                    false,
                ),
                predicate(
                    "DISLIKES",
                    "A person dislikes or avoids an entity, activity, topic, or object.",
                    Some("state"),
                    Some("many"),
                    Some(true),
                    false,
                ),
                predicate(
                    "VISITED",
                    "A person visited a location.",
                    Some("event"),
                    Some("many"),
                    Some(true),
                    false,
                ),
                predicate(
                    "ATTENDED",
                    "A person attended an event, meeting, appointment, or gathering.",
                    Some("event"),
                    Some("many"),
                    Some(true),
                    false,
                ),
                predicate(
                    "PARTICIPATED_IN",
                    "A person participated in an event, activity, project, or program.",
                    Some("event"),
                    Some("many"),
                    Some(true),
                    false,
                ),
                predicate(
                    "HAS_PREFERENCE",
                    "A person has an expressed preference, habit, or recurring choice.",
                    Some("state"),
                    Some("many"),
                    Some(true),
                    false,
                ),
                predicate(
                    "HAS_ATTRIBUTE",
                    "An entity has a stable attribute, role, status, or descriptive property.",
                    Some("state"),
                    Some("many"),
                    Some(true),
                    false,
                ),
                predicate(
                    "MENTIONED",
                    "A record or person mentions an entity, topic, event, or time.",
                    Some("event"),
                    Some("many"),
                    Some(true),
                    false,
                ),
                predicate(
                    GRAPH_FALLBACK_PREDICATE,
                    "A grounded relationship exists but does not fit a more specific registered predicate.",
                    Some("association"),
                    Some("many"),
                    Some(true),
                    false,
                ),
            ],
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

fn predicate(
    name: &str,
    description: &str,
    temporal_kind: Option<&str>,
    cardinality: Option<&str>,
    overlap_allowed: Option<bool>,
    symmetric: bool,
) -> GraphPredicate {
    GraphPredicate {
        name: name.to_string(),
        description: description.to_string(),
        temporal_kind: temporal_kind.map(ToOwned::to_owned),
        cardinality: cardinality.map(ToOwned::to_owned),
        overlap_allowed,
        symmetric,
    }
}
