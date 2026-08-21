use std::collections::HashMap;

use memory_pipeline::cache::JsonCache;
use memory_pipeline::episode::build_episodes;
use memory_pipeline::error::PipelineStage;
use memory_pipeline::extraction::StaticMemoryExtractor;
use memory_pipeline::grounding::StaticGroundingVerifier;
use memory_pipeline::normalize::normalize_prepared_memories;
use memory_pipeline::pipeline::{run_memory_pipeline, write_pipeline_artifacts, PipelineConfig};
use memory_pipeline::validation::{validate_extraction, ValidationConfig};
use memory_pipeline::window::{build_windows, WindowConfig};
use serde_json::json;

fn prepared() -> serde_json::Value {
    json!({
        "schema_version": "benchmark-prepared-v1",
        "dataset": {"name": "offline", "split": "test"},
        "memories": [{
            "id": "m1",
            "text": "I plan to move to Hangzhou in August.",
            "metadata": {
                "scope_id": "u1",
                "session_id": "s1",
                "role": "user",
                "speaker": "Alice",
                "timestamp": "2026-07-14T10:00:00Z"
            }
        }],
        "queries": [{"id": "q1", "text": "Where will Alice move?"}]
    })
}

fn raw_memory() -> serde_json::Value {
    json!({
        "text": "Alice plans to move to Hangzhou in August 2026.",
        "memory_type": "event",
        "subject": {"name": "Alice", "source_speaker": "Alice"},
        "predicate": "plans_to_move_to",
        "object": {"name": "Hangzhou", "type": "place"},
        "modality": "planned",
        "event_time": {"raw": "in August", "normalized": "2026-08", "precision": "month"},
        "attributes": {},
        "evidence": [{
            "message_id": "m1",
            "quote": "plan to move to Hangzhou in August",
            "evidence_role": "primary"
        }],
        "model_confidence": 0.9
    })
}

#[tokio::test]
async fn static_pipeline_writes_prepared_output_and_audit_bundle() {
    let source = prepared();
    let config = PipelineConfig {
        window: WindowConfig {
            max_candidate_tokens: 64,
            max_window_tokens: 128,
            ..WindowConfig::default()
        },
        ..PipelineConfig::default()
    };
    let (messages, _) = normalize_prepared_memories(&source).unwrap();
    let episodes = build_episodes(&messages, &config.episode).unwrap();
    let lookup = messages
        .into_iter()
        .map(|message| (message.id.clone(), message))
        .collect::<HashMap<_, _>>();
    let window = build_windows(&episodes, &lookup, &config.window)
        .unwrap()
        .remove(0);
    assert_eq!(episodes[0].id, "episode-2a64f6543acd7e8b042eb197");
    assert_eq!(window.id, "window-8cb3fa610f5713b5763306eb");
    let candidate = validate_extraction(
        &[raw_memory()],
        &window,
        &lookup,
        &ValidationConfig::default(),
    )
    .valid
    .remove(0);
    assert_eq!(candidate.id, "candidate-0d35d0af9cdf1ff776ddeae9");
    let extractor = StaticMemoryExtractor::new(HashMap::from([(
        window.id.clone(),
        json!({"schema_version": "atomic_memory_v1", "memories": [raw_memory()]}),
    )]));
    let verifier =
        StaticGroundingVerifier::new(HashMap::from([(candidate.id, json!("SUPPORTED"))]));

    let cache_temp = tempfile::tempdir().unwrap();
    let cache_root = cache_temp.path().to_path_buf();
    let cache = JsonCache::new(&cache_root, "cache_v1");
    let run = run_memory_pipeline(&source, &config, &extractor, &verifier, Some(&cache))
        .await
        .unwrap();

    assert_eq!(run.prepared["queries"], source["queries"]);
    assert_eq!(
        run.prepared["memories"][0]["metadata"]["memory_kind"],
        "extracted_memory"
    );
    assert_eq!(run.prepared["memories"][0]["metadata"]["speaker"], "Alice");
    assert_eq!(run.prepared["memories"][0]["metadata"]["session_id"], "s1");
    assert!(run.prepared["memories"][0]["metadata"]
        .get("turn_index")
        .is_none());
    assert_eq!(
        run.prepared["memories"][0]["metadata"]["observed_at_ms"],
        1_784_023_200_000i64
    );
    assert!(run.prepared["memories"][0]["id"]
        .as_str()
        .unwrap()
        .starts_with("mem-"));
    assert_eq!(
        run.prepared["memories"][0]["id"],
        "mem-0d35d0af9cdf1ff776ddeae9"
    );
    assert_eq!(run.stats["accepted_memory_count"], 1);
    assert_eq!(run.stats["grounding_status_counts"]["SUPPORTED"], 1);
    assert!(cache_root
        .join("extraction/a3bc787808310664d7006da1.json")
        .is_file());
    assert!(cache_root
        .join("grounding/5c0c29d957faf6d673d3e802.json")
        .is_file());
    let resumed = run_memory_pipeline(&source, &config, &extractor, &verifier, Some(&cache))
        .await
        .unwrap();
    assert_eq!(resumed.stats["extraction_cache_hits"], 1);
    assert_eq!(resumed.stats["verification_cache_hits"], 1);

    let temp = tempfile::tempdir().unwrap();
    write_pipeline_artifacts(&run, temp.path()).unwrap();
    write_pipeline_artifacts(&run, temp.path()).unwrap();
    for name in [
        "normalized_messages.jsonl",
        "episodes.jsonl",
        "extraction_windows.jsonl",
        "extracted_candidates.jsonl",
        "accepted_memories.jsonl",
        "rejected_extractions.jsonl",
        "quarantined_memories.jsonl",
        "extraction_stats.json",
        "run_metadata.json",
        "prepared.json",
    ] {
        assert!(temp.path().join(name).is_file(), "missing {name}");
    }
    assert!(std::fs::read_dir(temp.path()).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".tmp")));
}

#[tokio::test]
async fn candidate_coverage_excludes_context_only_sources() {
    let source = json!({
        "schema_version": "benchmark-prepared-v1",
        "memories": [
            {"id": "context", "text": "Earlier context.", "metadata": {
                "scope_id": "u1", "session_id": "s1", "role": "user",
                "memory_candidate": false}},
            {"id": "candidate", "text": "Current candidate.", "metadata": {
                "scope_id": "u1", "session_id": "s1", "role": "user",
                "memory_candidate": true}}
        ]
    });
    let config = PipelineConfig::default();
    let (messages, _) = normalize_prepared_memories(&source).unwrap();
    let episodes = build_episodes(&messages, &config.episode).unwrap();
    let lookup = messages
        .into_iter()
        .map(|message| (message.id.clone(), message))
        .collect::<HashMap<_, _>>();
    let window = build_windows(&episodes, &lookup, &config.window)
        .unwrap()
        .remove(0);
    let extractor = StaticMemoryExtractor::new(HashMap::from([(
        window.id,
        json!({"schema_version": "atomic_memory_v1", "memories": []}),
    )]));
    let verifier = StaticGroundingVerifier::new(HashMap::new());

    let run = run_memory_pipeline(&source, &config, &extractor, &verifier, None)
        .await
        .unwrap();

    assert_eq!(run.stats["candidate_source_coverage"], 1.0);
}

#[tokio::test]
async fn fail_fast_controls_extraction_and_grounding_failures() {
    let source = prepared();
    let config = PipelineConfig::default();
    let (messages, _) = normalize_prepared_memories(&source).unwrap();
    let episodes = build_episodes(&messages, &config.episode).unwrap();
    let lookup = messages
        .into_iter()
        .map(|message| (message.id.clone(), message))
        .collect::<HashMap<_, _>>();
    let window = build_windows(&episodes, &lookup, &config.window)
        .unwrap()
        .remove(0);

    let missing_extraction = StaticMemoryExtractor::new(HashMap::new());
    let empty_verifier = StaticGroundingVerifier::new(HashMap::new());
    let extraction_error =
        run_memory_pipeline(&source, &config, &missing_extraction, &empty_verifier, None)
            .await
            .unwrap_err();
    assert_eq!(extraction_error.stage(), Some(PipelineStage::Extract));

    let best_effort = PipelineConfig {
        fail_fast: false,
        ..config.clone()
    };
    let extraction_run = run_memory_pipeline(
        &source,
        &best_effort,
        &missing_extraction,
        &empty_verifier,
        None,
    )
    .await
    .unwrap();
    assert_eq!(extraction_run.rejected[0].stage, "extract");

    let extractor = StaticMemoryExtractor::new(HashMap::from([(
        window.id,
        json!({"schema_version": "atomic_memory_v1", "memories": [raw_memory()]}),
    )]));
    let grounding_error = run_memory_pipeline(&source, &config, &extractor, &empty_verifier, None)
        .await
        .unwrap_err();
    assert_eq!(grounding_error.stage(), Some(PipelineStage::Ground));

    let grounding_run =
        run_memory_pipeline(&source, &best_effort, &extractor, &empty_verifier, None)
            .await
            .unwrap();
    assert_eq!(grounding_run.quarantined[0].stage, "grounding");
    assert!(grounding_run.accepted_memories.is_empty());
}

#[tokio::test]
async fn only_supported_grounding_results_are_accepted() {
    for status in ["PARTIALLY_SUPPORTED", "UNSUPPORTED", "UNCERTAIN"] {
        let source = prepared();
        let config = PipelineConfig::default();
        let (messages, _) = normalize_prepared_memories(&source).unwrap();
        let episodes = build_episodes(&messages, &config.episode).unwrap();
        let lookup = messages
            .into_iter()
            .map(|message| (message.id.clone(), message))
            .collect::<HashMap<_, _>>();
        let window = build_windows(&episodes, &lookup, &config.window)
            .unwrap()
            .remove(0);
        let candidate = validate_extraction(
            &[raw_memory()],
            &window,
            &lookup,
            &ValidationConfig::default(),
        )
        .valid
        .remove(0);
        let extractor = StaticMemoryExtractor::new(HashMap::from([(
            window.id,
            json!({"schema_version": "atomic_memory_v1", "memories": [raw_memory()]}),
        )]));
        let verifier = StaticGroundingVerifier::new(HashMap::from([(candidate.id, json!(status))]));

        let run = run_memory_pipeline(&source, &config, &extractor, &verifier, None)
            .await
            .unwrap();

        assert!(run.accepted_memories.is_empty());
        assert_eq!(run.quarantined.len(), 1);
        assert_eq!(run.quarantined[0].details["status"], status);
    }
}

#[tokio::test]
async fn zero_max_memory_chars_is_rejected() {
    let mut config = PipelineConfig::default();
    config.validation.max_memory_chars = 0;
    let extractor = StaticMemoryExtractor::new(HashMap::new());
    let verifier = StaticGroundingVerifier::new(HashMap::new());

    let error = run_memory_pipeline(&prepared(), &config, &extractor, &verifier, None)
        .await
        .unwrap_err();

    assert!(error
        .to_string()
        .contains("max_memory_chars must be positive"));
}
