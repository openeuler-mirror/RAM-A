use std::collections::HashMap;
use std::process::Command;

use memory_pipeline::episode::{build_episodes, EpisodeConfig};
use memory_pipeline::normalize::normalize_prepared_memories;
use memory_pipeline::validation::{validate_extraction, ValidationConfig};
use memory_pipeline::window::{build_windows, WindowConfig};
use serde_json::json;

#[test]
fn fixture_cli_writes_prepared_and_artifacts() {
    let source = json!({
        "schema_version": "benchmark-prepared-v1",
        "dataset": {"name": "cli-fixture"},
        "memories": [{
            "id": "m1",
            "text": "I plan to move to Hangzhou in August.",
            "metadata": {"scope_id": "u1", "session_id": "s1", "role": "user"}
        }],
        "queries": []
    });
    let raw = json!({
        "text": "Alice plans to move to Hangzhou.", "memory_type": "event",
        "subject": {"name": "Alice"}, "predicate": "plans_to_move_to",
        "object": {"name": "Hangzhou"}, "modality": "planned",
        "event_time": null, "attributes": {},
        "evidence": [{"message_id": "m1", "quote": "plan to move to Hangzhou", "evidence_role": "primary"}],
        "model_confidence": 0.9
    });
    let config = WindowConfig {
        max_candidate_tokens: 64,
        max_window_tokens: 128,
        ..WindowConfig::default()
    };
    let (messages, _) = normalize_prepared_memories(&source).unwrap();
    let episodes = build_episodes(&messages, &EpisodeConfig::default()).unwrap();
    let lookup = messages
        .into_iter()
        .map(|message| (message.id.clone(), message))
        .collect::<HashMap<_, _>>();
    let window = build_windows(&episodes, &lookup, &config)
        .unwrap()
        .remove(0);
    let candidate = validate_extraction(
        std::slice::from_ref(&raw),
        &window,
        &lookup,
        &ValidationConfig::default(),
    )
    .valid
    .remove(0);
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input.json");
    let extraction = temp.path().join("extraction.json");
    let grounding = temp.path().join("grounding.json");
    let output = temp.path().join("out/prepared.json");
    let artifacts = temp.path().join("artifacts");
    std::fs::write(&input, serde_json::to_vec(&source).unwrap()).unwrap();
    std::fs::write(
        &extraction,
        serde_json::to_vec(
            &json!({window.id: {"schema_version": "atomic_memory_v1", "memories": [raw]}}),
        )
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        &grounding,
        serde_json::to_vec(&json!({candidate.id: "SUPPORTED"})).unwrap(),
    )
    .unwrap();

    let binary = std::env::var("CARGO_BIN_EXE_memory-pipeline").expect("memory-pipeline binary");
    let status = Command::new(binary)
        .args([
            "--input",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--artifacts-dir",
            artifacts.to_str().unwrap(),
            "--extractor-responses",
            extraction.to_str().unwrap(),
            "--grounding-responses",
            grounding.to_str().unwrap(),
            "--max-candidate-tokens",
            "64",
            "--max-window-tokens",
            "128",
        ])
        .status()
        .unwrap();
    assert!(status.success());
    let prepared: serde_json::Value =
        serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
    assert_eq!(
        prepared["memories"][0]["metadata"]["memory_kind"],
        "extracted_memory"
    );
    assert!(artifacts.join("prepared.json").is_file());
}
