//! 本文件验证 backends/ndjson/appender 模块的行为。

use std::{
    fs,
    io::{self, Write},
    sync::{Arc, Mutex},
    thread,
};

use actrail_kv_artifacts::{CapturedRequest, ComparisonMetadata};
use serde_json::json;
use tempfile::tempdir;

use super::{AppendError, AppendFile, NdjsonAppender};

#[test]
fn concurrent_appends_always_produce_complete_parseable_lines() {
    const WRITERS: usize = 24;
    const RECORDS_PER_WRITER: usize = 80;

    let directory = tempdir().expect("create temporary directory");
    let output = directory.path().join("requests.ndjson");
    let appender = Arc::new(NdjsonAppender::open(&output).expect("open appender"));

    let handles: Vec<_> = (0..WRITERS)
        .map(|writer| {
            let appender = Arc::clone(&appender);
            thread::spawn(move || {
                for record in 0..RECORDS_PER_WRITER {
                    appender
                        .append(&CapturedRequest {
                            captured_at: "2026-09-02T00:00:00Z".to_owned(),
                            source: Some(format!("writer-{writer}")),
                            session_key: Some(format!("session-{writer}")),
                            comparison: comparison(),
                            payload: json!({
                                "writer": writer,
                                "record": record,
                                "content": "包含换行标记而不是物理换行\n尾部"
                            }),
                        })
                        .expect("append complete record");
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("writer thread succeeds");
    }
    drop(appender);

    let contents = fs::read_to_string(output).expect("read output");
    let lines: Vec<_> = contents.lines().collect();
    assert_eq!(lines.len(), WRITERS * RECORDS_PER_WRITER);

    let mut seen = vec![vec![false; RECORDS_PER_WRITER]; WRITERS];
    for line in lines {
        let parsed: CapturedRequest = serde_json::from_str(line).expect("line is intact JSON");
        let writer = parsed.payload["writer"].as_u64().expect("writer") as usize;
        let record = parsed.payload["record"].as_u64().expect("record") as usize;
        seen[writer][record] = true;
    }
    assert!(seen.into_iter().flatten().all(|present| present));
}

#[test]
fn second_appender_cannot_open_the_same_output() {
    let directory = tempdir().expect("create temporary directory");
    let output = directory.path().join("requests.ndjson");
    let _first = NdjsonAppender::open(&output).expect("open first appender");

    assert!(matches!(
        NdjsonAppender::open(&output),
        Err(AppendError::AlreadyLocked(_))
    ));
}

#[test]
fn readers_can_read_output_while_appender_holds_the_lock() {
    // Analyzing the receiver's live output is a supported flow: the lock
    // must exclude a second receiver, never a reader.
    let directory = tempdir().expect("create temporary directory");
    let output = directory.path().join("requests.ndjson");
    let appender = NdjsonAppender::open(&output).expect("open appender");
    appender.append(&captured_record()).expect("append record");

    let contents = fs::read_to_string(&output).expect("read live output");
    let parsed: CapturedRequest =
        serde_json::from_str(contents.trim_end()).expect("line is intact JSON");
    assert_eq!(parsed.payload["model"], json!("example"));
}

#[cfg(unix)]
#[test]
fn creates_and_tightens_existing_output_to_owner_only_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempdir().expect("create temporary directory");
    let created = directory.path().join("created.ndjson");
    let created_appender = NdjsonAppender::open(&created).expect("create private output");
    assert_eq!(
        fs::metadata(&created)
            .expect("created metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    drop(created_appender);

    let existing = directory.path().join("existing.ndjson");
    fs::write(&existing, b"existing\n").expect("create existing output");
    fs::set_permissions(&existing, fs::Permissions::from_mode(0o666))
        .expect("make existing output permissive");
    let _existing_appender = NdjsonAppender::open(&existing).expect("open existing output");
    assert_eq!(
        fs::metadata(existing)
            .expect("existing metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn partial_write_is_rolled_back_and_permanently_fails_appender() {
    let shared = Arc::new(Mutex::new(FailingFileState {
        bytes: b"existing\n".to_vec(),
        fail_after: 5,
        truncate_fails: false,
    }));
    let appender = NdjsonAppender::from_output(Box::new(FailingFile {
        shared: Arc::clone(&shared),
        written: 0,
    }));
    let record = captured_record();

    assert!(matches!(appender.append(&record), Err(AppendError::Io(_))));
    assert_eq!(shared.lock().expect("state").bytes, b"existing\n");
    assert!(matches!(appender.append(&record), Err(AppendError::Failed)));
}

#[test]
fn rollback_failure_is_reported_and_permanently_fails_appender() {
    let shared = Arc::new(Mutex::new(FailingFileState {
        bytes: Vec::new(),
        fail_after: 3,
        truncate_fails: true,
    }));
    let appender = NdjsonAppender::from_output(Box::new(FailingFile { shared, written: 0 }));
    let record = captured_record();

    assert!(matches!(
        appender.append(&record),
        Err(AppendError::WriteAndRollback { .. })
    ));
    assert!(matches!(appender.append(&record), Err(AppendError::Failed)));
}

fn captured_record() -> CapturedRequest {
    CapturedRequest {
        captured_at: "2026-09-02T00:00:00Z".to_owned(),
        source: None,
        session_key: None,
        comparison: comparison(),
        payload: json!({"model": "example"}),
    }
}

fn comparison() -> ComparisonMetadata {
    ComparisonMetadata {
        endpoint_key: "llm-primary".to_owned(),
        agent_key: None,
        model_deployment_key: None,
        kv_namespace: None,
    }
}

struct FailingFile {
    shared: Arc<Mutex<FailingFileState>>,
    written: usize,
}

struct FailingFileState {
    bytes: Vec<u8>,
    fail_after: usize,
    truncate_fails: bool,
}

impl Write for FailingFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut state = self.shared.lock().expect("state");
        if self.written >= state.fail_after {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "injected failure"));
        }
        let accepted = bytes.len().min(state.fail_after - self.written);
        state.bytes.extend_from_slice(&bytes[..accepted]);
        self.written += accepted;
        Ok(accepted)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl AppendFile for FailingFile {
    fn len(&self) -> io::Result<u64> {
        Ok(self.shared.lock().expect("state").bytes.len() as u64)
    }

    fn truncate(&mut self, length: u64) -> io::Result<()> {
        let mut state = self.shared.lock().expect("state");
        if state.truncate_fails {
            return Err(io::Error::other("injected rollback failure"));
        }
        state.bytes.truncate(length as usize);
        Ok(())
    }
}
