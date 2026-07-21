use memory_core::graph::{
    stable_input_hash, FactLinkType, FactStatus, GraphInputHashFields, GraphTypeRegistry,
    MemorySpaceStatus,
};

#[test]
fn graph_enums_serialize_with_stable_snake_case() {
    assert_eq!(
        serde_json::to_string(&MemorySpaceStatus::Active).unwrap(),
        "\"active\""
    );
    assert_eq!(
        serde_json::to_string(&FactStatus::Superseded).unwrap(),
        "\"superseded\""
    );
    assert_eq!(
        serde_json::to_string(&FactLinkType::Contradicts).unwrap(),
        "\"contradicts\""
    );
}

#[test]
fn graph_registry_preserves_version_and_predicate_metadata() {
    let registry = GraphTypeRegistry::default();
    let predicate = registry
        .predicate("LIVES_IN")
        .expect("LIVES_IN predicate exists");

    assert_eq!(registry.version, "graph-type-registry-v2");
    assert_eq!(predicate.name, "LIVES_IN");
    assert_eq!(predicate.temporal_kind.as_deref(), Some("state"));
    assert_eq!(predicate.cardinality.as_deref(), Some("single"));
    assert_eq!(predicate.overlap_allowed, Some(false));
}

#[test]
fn graph_registry_covers_common_conversational_memory_predicates() {
    let registry = GraphTypeRegistry::default();

    for entity_type in [
        "PERSON",
        "LOCATION",
        "ORGANIZATION",
        "PROJECT",
        "EVENT",
        "ACTIVITY",
        "OBJECT",
        "PREFERENCE",
        "TIME",
        "GROUP",
        "CONCEPT",
    ] {
        assert!(
            registry
                .entity_types
                .iter()
                .any(|registered| registered == entity_type),
            "{entity_type} should be present in the default graph registry"
        );
    }

    for predicate in [
        "LIVES_IN",
        "WORKS_AT",
        "STUDIES_AT",
        "FAMILY_OF",
        "FRIEND_OF",
        "LIKES",
        "DISLIKES",
        "VISITED",
        "ATTENDED",
        "PARTICIPATED_IN",
        "HAS_PREFERENCE",
        "HAS_ATTRIBUTE",
        "MENTIONED",
        "RELATED_TO",
    ] {
        assert!(
            registry.predicate(predicate).is_some(),
            "{predicate} should be present in the default graph registry"
        );
    }
}

#[test]
fn input_hash_is_stable_for_semantic_fields() {
    let a = GraphInputHashFields {
        memory_space_id: "space-1".to_string(),
        session_id: Some("session-1".to_string()),
        session_sequence: Some(7),
        text: "User lives in Shanghai.".to_string(),
        source_kind: "conversation".to_string(),
        source_ref: Some("msg-7".to_string()),
        content_role: "user".to_string(),
        observed_at_ms: None,
        metadata: serde_json::json!({"scope_id": "space-1", "runtime_log": "ignored"}),
    };
    let b = GraphInputHashFields {
        metadata: serde_json::json!({"runtime_log": "changed", "scope_id": "space-1"}),
        ..a.clone()
    };

    assert_eq!(stable_input_hash(&a), stable_input_hash(&b));
}
