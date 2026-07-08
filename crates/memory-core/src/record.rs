use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
    pub embedding: Vec<f32>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

pub(crate) fn extract_scope_id(metadata: &serde_json::Value) -> Option<String> {
    metadata
        .get("scope_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

pub(crate) fn extract_scope_id_from_filter(filter: Option<&serde_json::Value>) -> Option<String> {
    filter
        .and_then(|value| value.get("scope_id"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

pub(crate) fn metadata_matches(
    metadata: &serde_json::Value,
    filter: Option<&serde_json::Value>,
) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    let Some(filter_object) = filter.as_object() else {
        return true;
    };
    let Some(metadata_object) = metadata.as_object() else {
        return false;
    };

    filter_object
        .iter()
        .all(|(key, expected)| metadata_object.get(key) == Some(expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_matches_requires_all_filter_fields() {
        let metadata = serde_json::json!({
            "scope_id": "scope-a",
            "kind": "preference",
        });

        assert!(metadata_matches(
            &metadata,
            Some(&serde_json::json!({"scope_id": "scope-a"})),
        ));
        assert!(metadata_matches(
            &metadata,
            Some(&serde_json::json!({
                "scope_id": "scope-a",
                "kind": "preference",
            })),
        ));
        assert!(!metadata_matches(
            &metadata,
            Some(&serde_json::json!({
                "scope_id": "scope-a",
                "kind": "fact",
            })),
        ));
    }

    #[test]
    fn extract_scope_id_reads_metadata_and_filter() {
        assert_eq!(
            extract_scope_id(&serde_json::json!({"scope_id": "scope-a"})).as_deref(),
            Some("scope-a"),
        );
        assert_eq!(
            extract_scope_id_from_filter(Some(&serde_json::json!({"scope_id": "scope-b"})))
                .as_deref(),
            Some("scope-b"),
        );
        assert_eq!(extract_scope_id(&serde_json::json!({})), None);
        assert_eq!(extract_scope_id_from_filter(None), None);
    }
}
