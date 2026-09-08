use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use memory_pipeline::episode::build_episodes;
use memory_pipeline::error::PipelineStage;
use memory_pipeline::extraction::StaticMemoryExtractor;
use memory_pipeline::grounding::StaticGroundingVerifier;
use memory_pipeline::normalize::normalize_prepared_memories;
use memory_pipeline::pipeline::{run_memory_pipeline, PipelineConfig};
use memory_pipeline::validation::{validate_extraction, ValidationConfig};
use memory_pipeline::window::build_windows;
use serde_json::{json, Value};
use tracing_subscriber::fmt::MakeWriter;

#[derive(Clone, Default)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

struct LogWriter(Arc<Mutex<Vec<u8>>>);

impl Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .expect("log buffer lock")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LogBuffer {
    type Writer = LogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogWriter(self.0.clone())
    }
}

impl LogBuffer {
    fn records(&self) -> Vec<Value> {
        let bytes = self.0.lock().expect("log buffer lock").clone();
        String::from_utf8(bytes)
            .expect("UTF-8 tracing output")
            .lines()
            .map(|line| serde_json::from_str(line).expect("JSON tracing record"))
            .collect()
    }

    fn text(&self) -> String {
        String::from_utf8(self.0.lock().expect("log buffer lock").clone())
            .expect("UTF-8 tracing output")
    }
}

fn prepared() -> Value {
    json!({
        "schema_version": "benchmark-prepared-v1",
        "dataset": {"name": "logging-test"},
        "memories": [{
            "id": "message-1",
            "text": "PRIVATE_PIPELINE_TEST Alice prefers tea.",
            "metadata": {
                "scope_id": "scope-1",
                "session_id": "conversation-1",
                "role": "user",
                "speaker": "Alice",
                "timestamp": "2026-08-17T10:00:00Z",
                "memory_candidate": true
            }
        }]
    })
}

fn raw_memory() -> Value {
    json!({
        "text": "Alice prefers tea.",
        "memory_type": "preference",
        "subject": {"name": "Alice", "source_speaker": "Alice"},
        "predicate": "prefers",
        "object": {"name": "tea", "type": "drink"},
        "modality": "asserted",
        "event_time": null,
        "attributes": {},
        "evidence": [{
            "message_id": "message-1",
            "quote": "Alice prefers tea.",
            "evidence_role": "primary"
        }],
        "model_confidence": 0.95
    })
}

fn successful_components(
    source: &Value,
    config: &PipelineConfig,
) -> (StaticMemoryExtractor, StaticGroundingVerifier) {
    let (messages, _) = normalize_prepared_memories(source).expect("normalize fixture");
    let episodes = build_episodes(&messages, &config.episode).expect("build episodes");
    let lookup = messages
        .into_iter()
        .map(|message| (message.id.clone(), message))
        .collect::<HashMap<_, _>>();
    let window = build_windows(&episodes, &lookup, &config.window)
        .expect("build windows")
        .remove(0);
    let candidate = validate_extraction(
        &[raw_memory()],
        &window,
        &lookup,
        &ValidationConfig::default(),
    )
    .valid
    .remove(0);
    (
        StaticMemoryExtractor::new(HashMap::from([(
            window.id,
            json!({"schema_version": "atomic_memory_v1", "memories": [raw_memory()]}),
        )])),
        StaticGroundingVerifier::new(HashMap::from([(candidate.id, json!("SUPPORTED"))])),
    )
}

fn event_stages(records: &[Value], event: &str) -> Vec<String> {
    records
        .iter()
        .filter_map(|record| {
            let fields = record.get("fields")?;
            (fields.get("event")?.as_str()? == event)
                .then(|| fields.get("stage")?.as_str().map(str::to_owned))
                .flatten()
        })
        .collect()
}

fn failure_codes(records: &[Value]) -> HashMap<String, String> {
    records
        .iter()
        .filter_map(|record| {
            let fields = record.get("fields")?;
            (fields.get("event")?.as_str()? == "ram_a.memory.ingest.stage.failed")
                .then(|| {
                    Some((
                        fields.get("stage")?.as_str()?.to_owned(),
                        fields.get("error_code")?.as_str()?.to_owned(),
                    ))
                })
                .flatten()
        })
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn successful_pipeline_logs_all_seven_stages_without_memory_content() {
    let logs = LogBuffer::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .without_time()
        .with_max_level(tracing::Level::INFO)
        .with_writer(logs.clone())
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);
    let source = prepared();
    let config = PipelineConfig::default();
    let (extractor, verifier) = successful_components(&source, &config);

    run_memory_pipeline(&source, &config, &extractor, &verifier, None)
        .await
        .expect("successful pipeline");

    let records = logs.records();
    let expected = vec![
        "normalize",
        "episode",
        "window",
        "extract",
        "validate",
        "ground",
        "aggregate",
    ];
    assert_eq!(
        event_stages(&records, "ram_a.memory.ingest.stage.started"),
        expected
    );
    assert_eq!(
        event_stages(&records, "ram_a.memory.ingest.stage.completed"),
        expected
    );
    assert!(!logs.text().contains("PRIVATE_PIPELINE_TEST"));
    assert!(!logs.text().contains("Alice prefers tea."));
}

#[tokio::test(flavor = "current_thread")]
async fn pipeline_logs_every_reachable_fatal_stage_failure() {
    let logs = LogBuffer::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .without_time()
        .with_max_level(tracing::Level::INFO)
        .with_writer(logs.clone())
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);
    let source = prepared();
    let empty_extractor = StaticMemoryExtractor::new(HashMap::new());
    let empty_verifier = StaticGroundingVerifier::new(HashMap::new());

    let mut invalid_validation = PipelineConfig::default();
    invalid_validation.validation.max_memory_chars = 0;
    let error = run_memory_pipeline(
        &source,
        &invalid_validation,
        &empty_extractor,
        &empty_verifier,
        None,
    )
    .await
    .expect_err("invalid validation config");
    assert_eq!(error.stage(), Some(PipelineStage::Validate));

    let error = run_memory_pipeline(
        &json!({"schema_version": "wrong"}),
        &PipelineConfig::default(),
        &empty_extractor,
        &empty_verifier,
        None,
    )
    .await
    .expect_err("invalid normalize input");
    assert_eq!(error.stage(), Some(PipelineStage::Normalize));

    let mut invalid_episode = PipelineConfig::default();
    invalid_episode.episode.max_time_gap_minutes = Some(-1);
    let error = run_memory_pipeline(
        &source,
        &invalid_episode,
        &empty_extractor,
        &empty_verifier,
        None,
    )
    .await
    .expect_err("invalid episode config");
    assert_eq!(error.stage(), Some(PipelineStage::Episode));

    let mut invalid_window = PipelineConfig::default();
    invalid_window.window.max_candidate_tokens = 0;
    let error = run_memory_pipeline(
        &source,
        &invalid_window,
        &empty_extractor,
        &empty_verifier,
        None,
    )
    .await
    .expect_err("invalid window config");
    assert_eq!(error.stage(), Some(PipelineStage::Window));

    let config = PipelineConfig::default();
    let error = run_memory_pipeline(&source, &config, &empty_extractor, &empty_verifier, None)
        .await
        .expect_err("extractor failure");
    assert_eq!(error.stage(), Some(PipelineStage::Extract));

    let (extractor, _) = successful_components(&source, &config);
    let error = run_memory_pipeline(&source, &config, &extractor, &empty_verifier, None)
        .await
        .expect_err("grounding failure");
    assert_eq!(error.stage(), Some(PipelineStage::Ground));

    let failures = failure_codes(&logs.records());
    for (stage, code) in [
        ("validate", "PIPELINE_VALIDATE_FAILED"),
        ("normalize", "PIPELINE_NORMALIZE_FAILED"),
        ("episode", "PIPELINE_EPISODE_FAILED"),
        ("window", "PIPELINE_WINDOW_FAILED"),
        ("extract", "PIPELINE_EXTRACT_FAILED"),
        ("ground", "PIPELINE_GROUND_FAILED"),
    ] {
        assert_eq!(failures.get(stage).map(String::as_str), Some(code));
    }
    assert!(!failures.contains_key("aggregate"));
}
