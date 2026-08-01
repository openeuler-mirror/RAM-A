use std::collections::HashMap;

use memory_pipeline::canonical::{estimate_tokens, stable_hash};
use memory_pipeline::episode::{build_episodes, EpisodeConfig};
use memory_pipeline::normalize::normalize_prepared_memories;
use memory_pipeline::window::{build_windows, WindowConfig};
use serde_json::json;

fn prepared() -> serde_json::Value {
    json!({
        "schema_version": "benchmark-prepared-v1",
        "dataset": {"name": "fixture"},
        "memories": [
            {
                "id": "m1",
                "text": "你好。再见。",
                "metadata": {
                    "scope_id": "u1",
                    "session_id": "s1",
                    "role": "user",
                    "speaker": "Alice",
                    "timestamp": "2026-07-21T10:00:00Z"
                }
            },
            {
                "id": "m2",
                "text": "I like tea.",
                "metadata": {
                    "scope_id": "u1",
                    "session_id": "s2",
                    "role": "user",
                    "timestamp": "2026-07-21T10:01:00Z"
                }
            }
        ],
        "queries": []
    })
}

fn prepared_with_flags(flags: &[(&str, bool)]) -> serde_json::Value {
    json!({
        "schema_version": "benchmark-prepared-v1",
        "memories": flags
            .iter()
            .map(|(id, memory_candidate)| json!({
                "id": id,
                "text": format!("message {id}"),
                "metadata": {
                    "scope_id": "u1",
                    "session_id": "s1",
                    "role": "user",
                    "memory_candidate": memory_candidate
                }
            }))
            .collect::<Vec<_>>()
    })
}

fn windows_for(
    messages: Vec<memory_pipeline::models::NormalizedMessage>,
) -> Vec<memory_pipeline::models::ExtractionWindow> {
    let episodes = build_episodes(&messages, &EpisodeConfig::default()).unwrap();
    let lookup = messages
        .into_iter()
        .map(|message| (message.id.clone(), message))
        .collect::<HashMap<_, _>>();
    build_windows(&episodes, &lookup, &WindowConfig::default()).unwrap()
}

#[test]
fn canonical_helpers_match_python_contract() {
    assert_eq!(
        stable_hash(&[json!("你好"), json!({"b": 2, "a": 1})]),
        "f6bad747ec285ba27e28e7fc"
    );
    assert_eq!(estimate_tokens("Hello, 世界!"), 5);
    assert_eq!(
        stable_hash(&[json!([1e-6, 1e20, -0.0, 1.0])]),
        "9e3dd2f72d16f9fe18bcf07d"
    );
    let large: serde_json::Value = serde_json::from_str("123456789012345678901234567890").unwrap();
    assert_eq!(
        memory_pipeline::canonical::canonical_json(&large),
        "123456789012345678901234567890"
    );
    let overflow: serde_json::Value = serde_json::from_str("1e400").unwrap();
    assert_eq!(
        memory_pipeline::canonical::canonical_json(&overflow),
        "1e+400"
    );
}

#[test]
fn cache_round_trips_arbitrary_precision_numbers() {
    let temp = tempfile::tempdir().unwrap();
    let cache = memory_pipeline::cache::JsonCache::new(temp.path(), "cache_v1");
    let value: serde_json::Value = serde_json::from_str(r#"{"subject":{"weight":1e400}}"#).unwrap();

    cache.put("extraction", &[json!("key")], &value).unwrap();

    assert_eq!(
        cache.get("extraction", &[json!("key")]).unwrap(),
        Some(value)
    );
}

#[test]
fn normalization_and_episode_boundaries_preserve_contract() {
    let (messages, issues) = normalize_prepared_memories(&prepared()).unwrap();
    assert!(issues.is_empty());
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].source_index, 0);
    assert_eq!(messages[0].speaker, "Alice");

    let episodes = build_episodes(&messages, &EpisodeConfig::default()).unwrap();
    assert_eq!(episodes.len(), 2);
    assert_eq!(episodes[0].boundary_reason, "start");
    assert_eq!(episodes[1].boundary_reason, "session_change");
    assert_eq!(episodes[0].message_ids, vec!["m1"]);
}

#[test]
fn missing_candidate_flag_preserves_batch_behavior() {
    let (messages, _) = normalize_prepared_memories(&prepared()).unwrap();
    assert!(messages[0].candidate_eligible);
}

#[test]
fn null_candidate_flag_preserves_batch_behavior() {
    let mut prepared = prepared();
    prepared["memories"][0]["metadata"]["memory_candidate"] = serde_json::Value::Null;
    let (messages, _) = normalize_prepared_memories(&prepared).unwrap();
    assert!(messages[0].candidate_eligible);
}

#[test]
fn context_only_message_is_never_a_candidate_but_can_be_context() {
    let prepared = prepared_with_flags(&[("m1", false), ("m2", true)]);
    let (messages, _) = normalize_prepared_memories(&prepared).unwrap();
    let windows = windows_for(messages);

    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].candidate_message_ids, vec!["m2"]);
    assert_eq!(windows[0].context_before_refs[0].message_id, "m1");
}

#[test]
fn context_only_message_between_candidates_is_retained_as_context() {
    let prepared = prepared_with_flags(&[("m1", true), ("m2", false), ("m3", true)]);
    let (messages, _) = normalize_prepared_memories(&prepared).unwrap();
    let windows = windows_for(messages);

    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].candidate_message_ids, vec!["m1", "m3"]);
    assert!(windows[0]
        .context_before_refs
        .iter()
        .chain(&windows[0].context_after_refs)
        .any(|reference| reference.message_id == "m2"));
    assert!(windows[0]
        .candidate_refs
        .iter()
        .all(|reference| reference.message_id != "m2"));
}

#[test]
fn episode_with_only_context_only_messages_emits_no_windows() {
    let prepared = prepared_with_flags(&[("m1", false)]);
    let (messages, _) = normalize_prepared_memories(&prepared).unwrap();
    assert!(windows_for(messages).is_empty());
}

#[test]
fn duplicate_source_ids_are_fatal() {
    let mut value = prepared();
    value["memories"][1]["id"] = json!("m1");
    let error = normalize_prepared_memories(&value).unwrap_err();
    assert!(error
        .to_string()
        .contains("duplicate source message id: m1"));
}

#[test]
fn window_offsets_are_unicode_character_offsets() {
    let (messages, _) = normalize_prepared_memories(&prepared()).unwrap();
    let episodes = build_episodes(&messages[..1], &EpisodeConfig::default()).unwrap();
    let lookup = messages
        .into_iter()
        .map(|message| (message.id.clone(), message))
        .collect::<HashMap<_, _>>();
    let config = WindowConfig {
        max_candidate_tokens: 3,
        max_window_tokens: 6,
        context_before_messages: 0,
        context_after_messages: 0,
        ..WindowConfig::default()
    };

    let windows = build_windows(&episodes, &lookup, &config).unwrap();

    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].candidate_refs[0].start_char, 0);
    assert_eq!(windows[0].candidate_refs[0].end_char, 3);
    assert_eq!(windows[0].candidate_refs[0].text, "你好。");
    assert_eq!(windows[1].candidate_refs[0].start_char, 3);
    assert_eq!(windows[1].candidate_refs[0].end_char, 6);
    assert_eq!(windows[1].candidate_refs[0].text, "再见。");
}

#[test]
fn english_sentence_span_includes_python_compatible_trailing_space() {
    let value = json!({
        "schema_version": "benchmark-prepared-v1",
        "memories": [{
            "id": "english",
            "text": "One. Two.",
            "metadata": {"scope_id": "u1", "session_id": "s1", "role": "user"}
        }]
    });
    let (messages, _) = normalize_prepared_memories(&value).unwrap();
    let episodes = build_episodes(&messages, &EpisodeConfig::default()).unwrap();
    let lookup = messages
        .into_iter()
        .map(|message| (message.id.clone(), message))
        .collect::<HashMap<_, _>>();
    let windows = build_windows(
        &episodes,
        &lookup,
        &WindowConfig {
            max_candidate_tokens: 2,
            max_window_tokens: 2,
            context_before_messages: 0,
            context_after_messages: 0,
            ..WindowConfig::default()
        },
    )
    .unwrap();

    assert_eq!(windows[0].candidate_refs[0].text, "One. ");
    assert_eq!(windows[0].candidate_refs[0].end_char, 5);
    assert_eq!(windows[1].candidate_refs[0].text, "Two.");
    assert_eq!(windows[1].candidate_refs[0].start_char, 5);
}

#[test]
fn episode_time_gap_accepts_python_iso_space_separator() {
    let value = json!({
        "schema_version": "benchmark-prepared-v1",
        "memories": [
            {"id": "m1", "text": "one", "metadata": {"scope_id": "u1", "session_id": "s1", "timestamp": "2026-07-21 10:00:00"}},
            {"id": "m2", "text": "two", "metadata": {"scope_id": "u1", "session_id": "s1", "timestamp": "2026-07-21 12:00:00"}}
        ]
    });
    let (messages, _) = normalize_prepared_memories(&value).unwrap();
    let episodes = build_episodes(
        &messages,
        &EpisodeConfig {
            max_time_gap_minutes: Some(30),
            ..EpisodeConfig::default()
        },
    )
    .unwrap();

    assert_eq!(episodes.len(), 2);
    assert_eq!(episodes[1].boundary_reason, "time_gap");
}

#[test]
fn episode_boundaries_normalize_null_and_parse_offset_or_date_only_times() {
    let value = json!({
        "schema_version": "benchmark-prepared-v1",
        "memories": [
            {"id": "m1", "text": "one", "metadata": {"scope_id": "u1", "session_id": "s1", "timestamp": "2026-07-21", "topic": null}},
            {"id": "m2", "text": "two", "metadata": {"scope_id": "u1", "session_id": "s1", "timestamp": "2026-07-21 10:00:00+08:00"}}
        ]
    });
    let (messages, _) = normalize_prepared_memories(&value).unwrap();
    let episodes = build_episodes(
        &messages,
        &EpisodeConfig {
            max_time_gap_minutes: Some(30),
            metadata_boundary_fields: vec!["topic".into()],
            ..EpisodeConfig::default()
        },
    )
    .unwrap();

    assert_eq!(episodes.len(), 2);
    assert_eq!(episodes[1].boundary_reason, "time_gap");
}
