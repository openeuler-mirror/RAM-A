use std::collections::HashMap;

use memory_pipeline::episode::{build_episodes, EpisodeConfig};
use memory_pipeline::extraction::{parse_extraction_json, MemoryExtractor, StaticMemoryExtractor};
use memory_pipeline::grounding::{GroundingVerifier, StaticGroundingVerifier};
use memory_pipeline::models::AtomicMemory;
use memory_pipeline::normalize::normalize_prepared_memories;
use memory_pipeline::validation::{validate_extraction, ValidationConfig};
use memory_pipeline::window::{build_windows, WindowConfig};
use memory_pipeline::writer::{
    aggregate_exact_memories, attach_source_observations, make_prepared_output,
};
use serde_json::{json, Map, Value};

fn setup() -> (
    memory_pipeline::models::ExtractionWindow,
    HashMap<String, memory_pipeline::models::NormalizedMessage>,
) {
    let prepared = json!({
        "schema_version": "benchmark-prepared-v1",
        "memories": [{
            "id": "m1",
            "text": "我计划去杭州。",
            "metadata": {
                "scope_id": "u1",
                "session_id": "s1",
                "role": "user",
                "speaker": "Alice",
                "timestamp": "2026-07-21T10:00:00Z",
                "turn_index": 7
            }
        }]
    });
    let (messages, _) = normalize_prepared_memories(&prepared).unwrap();
    let episodes = build_episodes(&messages, &EpisodeConfig::default()).unwrap();
    let lookup = messages
        .into_iter()
        .map(|message| (message.id.clone(), message))
        .collect::<HashMap<_, _>>();
    let window = build_windows(&episodes, &lookup, &WindowConfig::default())
        .unwrap()
        .remove(0);
    (window, lookup)
}

fn raw_memory(modality: &str) -> Value {
    json!({
        "text": "Alice plans to go to Hangzhou.",
        "memory_type": "event",
        "subject": {"name": "Alice"},
        "predicate": "plans_to_visit",
        "object": {"name": "Hangzhou"},
        "modality": modality,
        "event_time": null,
        "attributes": {},
        "evidence": [{
            "message_id": "m1",
            "quote": "计划去杭州",
            "evidence_role": "primary"
        }],
        "model_confidence": 0.9
    })
}

#[test]
fn fenced_extraction_json_is_accepted() {
    let payload = "{\"schema_version\":\"atomic_memory_v1\",\"memories\":[]}";
    for content in [
        format!("```json\n{payload}\n```"),
        format!("```JSON\r\n{payload}\r\n```\r\n"),
        format!("```\r\n{payload}\r\n```"),
        format!("```json{payload}```"),
    ] {
        let parsed = parse_extraction_json(&content).unwrap();
        assert_eq!(parsed["schema_version"], "atomic_memory_v1");
    }
}

#[test]
fn evidence_quote_can_cross_adjacent_slices_of_one_message() {
    let prepared = json!({
        "schema_version": "benchmark-prepared-v1",
        "memories": [{
            "id": "m1", "text": "甲。乙。丙。",
            "metadata": {"scope_id": "u1", "session_id": "s1", "role": "user"}
        }]
    });
    let (messages, _) = normalize_prepared_memories(&prepared).unwrap();
    let episodes = build_episodes(&messages, &EpisodeConfig::default()).unwrap();
    let lookup = messages
        .into_iter()
        .map(|message| (message.id.clone(), message))
        .collect::<HashMap<_, _>>();
    let window = build_windows(
        &episodes,
        &lookup,
        &WindowConfig {
            max_candidate_tokens: 4,
            max_window_tokens: 4,
            context_before_messages: 0,
            context_after_messages: 0,
            ..WindowConfig::default()
        },
    )
    .unwrap()
    .remove(0);
    assert_eq!(window.candidate_refs.len(), 2);
    let raw = json!({
        "text": "甲之后是乙。", "memory_type": "fact",
        "subject": {"name": "sequence"}, "predicate": "contains", "object": "。乙",
        "modality": "asserted", "event_time": null, "attributes": {},
        "evidence": [{"message_id": "m1", "quote": "。乙", "evidence_role": "primary"}]
    });

    let batch = validate_extraction(&[raw], &window, &lookup, &ValidationConfig::default());

    assert!(batch.rejected.is_empty());
    assert!(batch.quarantined.is_empty());
    assert_eq!(batch.valid[0].evidence[0].start_char, 1);
    assert_eq!(batch.valid[0].evidence[0].end_char, 3);
}

#[test]
fn aggregation_compares_observation_times_as_instants() {
    let memory = |observed_at: &str, window: &str| AtomicMemory {
        id: format!("candidate-{window}"),
        scope_id: "u1".into(),
        text: "Alice likes tea.".into(),
        memory_type: "preference".into(),
        subject: Map::from_iter([("name".into(), json!("Alice"))]),
        predicate: "likes".into(),
        object: Some(json!("tea")),
        modality: "asserted".into(),
        evidence: Vec::new(),
        event_time: None,
        attributes: Map::new(),
        model_confidence: None,
        observed_at: observed_at.into(),
        source_episode_id: "episode-1".into(),
        source_window_id: window.into(),
        observation_refs: Vec::new(),
    };
    let mut later = memory("2024-01-02T02:04:05Z", "window-1");
    later.observation_refs = vec![Map::from_iter([
        ("observed_at".into(), json!(later.observed_at)),
        ("speaker".into(), json!("Alice")),
        ("session_id".into(), json!("s2")),
        ("turn_index".into(), json!(4)),
    ])];
    let mut earlier = memory("2024-01-02T03:04:05+08:00", "window-2");
    earlier.observation_refs = vec![Map::from_iter([
        ("observed_at".into(), json!(earlier.observed_at)),
        ("speaker".into(), json!("Bob")),
        ("session_id".into(), json!("s1")),
        ("turn_index".into(), json!(2)),
    ])];

    let aggregated = aggregate_exact_memories(&[later, earlier]);

    assert_eq!(aggregated[0].observed_at, "2024-01-02T02:04:05Z");
    assert_eq!(aggregated[0].observation_refs.len(), 2);
}

#[test]
fn validation_uses_unicode_offsets_and_preserves_planned_modality() {
    let (window, lookup) = setup();
    let batch = validate_extraction(
        &[raw_memory("planned")],
        &window,
        &lookup,
        &ValidationConfig::default(),
    );
    assert!(batch.rejected.is_empty());
    assert!(batch.quarantined.is_empty());
    assert_eq!(batch.valid.len(), 1);
    assert_eq!(batch.valid[0].evidence[0].start_char, 1);
    assert_eq!(batch.valid[0].evidence[0].end_char, 6);
    assert_eq!(batch.valid[0].observed_at, "2026-07-21T10:00:00Z");
    let mut valid = batch.valid.clone();
    attach_source_observations(&mut valid, &lookup);
    let prepared = make_prepared_output(
        &json!({"schema_version": "benchmark-prepared-v1"}),
        &valid,
        &json!({}),
    )
    .unwrap();
    let metadata = &prepared["memories"][0]["metadata"];
    assert_eq!(metadata["speaker"], "Alice");
    assert_eq!(metadata["session_id"], "s1");
    assert_eq!(metadata["turn_index"], 7);
    assert_eq!(metadata["observed_at_ms"], 1_784_628_000_000i64);
}

#[test]
fn asserted_plan_is_quarantined() {
    let (window, lookup) = setup();
    let batch = validate_extraction(
        &[raw_memory("asserted")],
        &window,
        &lookup,
        &ValidationConfig::default(),
    );
    assert!(batch.valid.is_empty());
    assert_eq!(batch.quarantined[0].code, "suspicious_modality");
}

#[test]
fn empty_event_time_becomes_null_and_integer_confidence_is_preserved() {
    let (window, lookup) = setup();
    let mut raw = raw_memory("planned");
    raw["event_time"] = json!({});
    raw["model_confidence"] = json!(1);

    let memory = validate_extraction(&[raw], &window, &lookup, &ValidationConfig::default())
        .valid
        .remove(0);

    assert!(memory.event_time.is_none());
    assert_eq!(memory.model_confidence, Some(json!(1)));
}

#[test]
fn overlapping_evidence_matches_are_quarantined_as_ambiguous() {
    let prepared = json!({
        "schema_version": "benchmark-prepared-v1",
        "memories": [{
            "id": "m1", "text": "aaa",
            "metadata": {"scope_id": "u1", "session_id": "s1", "role": "user"}
        }]
    });
    let (messages, _) = normalize_prepared_memories(&prepared).unwrap();
    let episodes = build_episodes(&messages, &EpisodeConfig::default()).unwrap();
    let lookup = messages
        .into_iter()
        .map(|message| (message.id.clone(), message))
        .collect::<HashMap<_, _>>();
    let window = build_windows(&episodes, &lookup, &WindowConfig::default())
        .unwrap()
        .remove(0);
    let raw = json!({
        "text": "The source contains aa.", "memory_type": "fact",
        "subject": {"name": "source"}, "predicate": "contains", "object": "aa",
        "modality": "asserted", "event_time": null, "attributes": {},
        "evidence": [{"message_id": "m1", "quote": "aa", "evidence_role": "primary"}]
    });

    let batch = validate_extraction(&[raw], &window, &lookup, &ValidationConfig::default());

    assert!(batch.valid.is_empty());
    assert_eq!(batch.quarantined[0].code, "ambiguous_evidence_quote");
}

#[test]
fn observed_at_prefers_last_evidence_timestamp_including_context() {
    let prepared = json!({
        "schema_version": "benchmark-prepared-v1",
        "memories": [
            {"id": "context", "text": "Alice moved.", "metadata": {
                "scope_id": "u1", "session_id": "s1", "role": "user",
                "speaker": "Alice", "turn_index": 4,
                "timestamp": "2026-07-22T10:00:00Z"}},
            {"id": "candidate", "text": "She likes tea.", "metadata": {
                "scope_id": "u1", "session_id": "s1", "role": "user",
                "speaker": "Bob", "turn_index": 5,
                "timestamp": "2026-07-21T10:00:00Z"}}
        ]
    });
    let (messages, _) = normalize_prepared_memories(&prepared).unwrap();
    let episodes = build_episodes(&messages, &EpisodeConfig::default()).unwrap();
    let lookup = messages
        .into_iter()
        .map(|message| (message.id.clone(), message))
        .collect::<HashMap<_, _>>();
    let windows = build_windows(
        &episodes,
        &lookup,
        &WindowConfig {
            max_candidate_tokens: 3,
            max_window_tokens: 8,
            context_before_messages: 1,
            ..WindowConfig::default()
        },
    )
    .unwrap();
    let window = windows
        .iter()
        .find(|window| window.candidate_message_ids == ["candidate"])
        .unwrap();
    let raw = json!({
        "text": "Alice likes tea after moving.", "memory_type": "preference",
        "subject": {"name": "Alice"}, "predicate": "likes", "object": "tea",
        "modality": "asserted", "event_time": null, "attributes": {},
        "evidence": [
            {"message_id": "candidate", "quote": "likes tea", "evidence_role": "primary"},
            {"message_id": "candidate", "quote": "She", "evidence_role": "supporting"},
            {"message_id": "context", "quote": "Alice moved", "evidence_role": "supporting"}
        ]
    });

    let mut batch = validate_extraction(&[raw], window, &lookup, &ValidationConfig::default());

    assert_eq!(batch.valid[0].observed_at, "2026-07-22T10:00:00Z");
    attach_source_observations(&mut batch.valid, &lookup);
    let observations = &batch.valid[0].observation_refs;
    assert_eq!(observations.len(), 2);
    let candidate = observations
        .iter()
        .find(|observation| observation["evidence_refs"][0]["message_id"] == "candidate")
        .unwrap();
    assert_eq!(candidate["speaker"], "Bob");
    assert_eq!(candidate["turn_index"], 5);
    assert_eq!(candidate["observed_at"], "2026-07-21T10:00:00Z");
    assert_eq!(candidate["evidence_refs"].as_array().unwrap().len(), 2);
    let context = observations
        .iter()
        .find(|observation| observation["evidence_refs"][0]["message_id"] == "context")
        .unwrap();
    assert_eq!(context["speaker"], "Alice");
    assert_eq!(context["turn_index"], 4);
    assert_eq!(context["observed_at"], "2026-07-22T10:00:00Z");
    assert_eq!(context["evidence_refs"].as_array().unwrap().len(), 1);

    let aggregated = aggregate_exact_memories(&batch.valid);
    let prepared = make_prepared_output(
        &json!({"schema_version": "benchmark-prepared-v1"}),
        &aggregated,
        &json!({}),
    )
    .unwrap();
    let metadata = &prepared["memories"][0]["metadata"];
    assert_eq!(metadata["speaker"], "Alice");
    assert_eq!(metadata["turn_index"], 4);
    assert_eq!(metadata["observed_at"], "2026-07-22T10:00:00Z");
    assert_eq!(metadata["observed_at_ms"], 1_784_714_400_000_i64);
}

#[test]
fn context_only_primary_evidence_is_rejected() {
    let prepared = json!({
        "schema_version": "benchmark-prepared-v1",
        "memories": [
            {"id": "context", "text": "Alice moved.", "metadata": {
                "scope_id": "u1", "session_id": "s1", "role": "user",
                "memory_candidate": false}},
            {"id": "candidate", "text": "She likes tea.", "metadata": {
                "scope_id": "u1", "session_id": "s1", "role": "user",
                "memory_candidate": true}}
        ]
    });
    let (messages, _) = normalize_prepared_memories(&prepared).unwrap();
    let episodes = build_episodes(&messages, &EpisodeConfig::default()).unwrap();
    let lookup = messages
        .into_iter()
        .map(|message| (message.id.clone(), message))
        .collect::<HashMap<_, _>>();
    let window = build_windows(&episodes, &lookup, &WindowConfig::default())
        .unwrap()
        .remove(0);
    let raw = json!({
        "text": "Alice moved.", "memory_type": "event",
        "subject": {"name": "Alice"}, "predicate": "moved", "object": null,
        "modality": "asserted", "event_time": null, "attributes": {},
        "evidence": [{
            "message_id": "context", "quote": "Alice moved",
            "evidence_role": "primary"
        }]
    });

    let batch = validate_extraction(&[raw], &window, &lookup, &ValidationConfig::default());

    assert!(batch.valid.is_empty());
    assert_eq!(batch.rejected[0].code, "missing_candidate_evidence");
}

#[tokio::test]
async fn static_extraction_and_grounding_share_the_product_path() {
    let (window, lookup) = setup();
    let extractor = StaticMemoryExtractor::new(HashMap::from([(
        window.id.clone(),
        json!({
            "schema_version": "atomic_memory_v1",
            "memories": [raw_memory("planned")]
        }),
    )]));
    let extraction = extractor.extract(&window, &lookup).await.unwrap();
    let validation = validate_extraction(
        &extraction.raw_memories,
        &window,
        &lookup,
        &ValidationConfig::default(),
    );
    let verifier = StaticGroundingVerifier::new(HashMap::from([(
        validation.valid[0].id.clone(),
        json!("SUPPORTED"),
    )]));
    let grounded = verifier
        .verify(&window, &validation.valid, &lookup)
        .await
        .unwrap();
    assert_eq!(grounded.results[0].status, "SUPPORTED");
}
