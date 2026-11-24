//! 验证后端选择、筛选、快照、保留期及 NDJSON 轮转恢复。

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration as StdDuration,
};

use actrail_kv_artifacts::{CapturedRequest, ComparisonMetadata};
use actrail_kv_storage::{open_store, CaptureFilter, StorageConfig, StorageError, StoreAccess};
use futures_util::TryStreamExt;
use serde_json::{json, Value};
use time::{format_description::well_known::Rfc3339, Duration, OffsetDateTime};

#[test]
fn switches_backend_and_retention_by_changing_only_the_selector() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("storage.json");
    let mut document = json!({
        "storage": {
            "backend": "sqlite",
            "retention": {"max_age_seconds": 604800},
            "backends": {
                "sqlite": {
                    "path": "captures.sqlite",
                    "retention": {"max_age_seconds": 86400, "delete_batch_size": 128}
                },
                "ndjson": {
                    "directory": "segments",
                    "retention": {"cleanup_interval_seconds": 30, "max_segments_per_cleanup": 3}
                }
            }
        }
    });
    fs::write(&path, document.to_string()).unwrap();
    let StorageConfig::Sqlite(sqlite) = StorageConfig::from_file(&path).unwrap() else {
        panic!("expected SQLite");
    };
    assert_eq!(sqlite.path, directory.path().join("captures.sqlite"));
    assert_eq!(sqlite.retention.unwrap().max_age_seconds.get(), 86400);
    assert_eq!(sqlite.delete_batch_size.get(), 128);

    document["storage"]["backend"] = json!("ndjson");
    fs::write(&path, document.to_string()).unwrap();
    let StorageConfig::Ndjson(ndjson) = StorageConfig::from_file(&path).unwrap() else {
        panic!("expected NDJSON");
    };
    assert_eq!(ndjson.directory, directory.path().join("segments"));
    assert_eq!(ndjson.retention.unwrap().max_age_seconds.get(), 604800);
    assert_eq!(ndjson.retention.unwrap().cleanup_interval_seconds.get(), 30);
    assert_eq!(ndjson.max_segments_per_cleanup.unwrap().get(), 3);
}

#[test]
fn retention_execution_options_belong_to_their_backend() {
    let mut document = json!({"storage": {
        "backend": "sqlite",
        "retention": {"max_age_seconds": 60, "cleanup_interval_seconds": 1},
        "backends": {"sqlite": {"path": "captures.sqlite"}}
    }});
    assert!(serde_json::from_value::<StorageConfig>(document.clone())
        .unwrap_err()
        .to_string()
        .contains("unknown field `cleanup_interval_seconds`"));
    document["storage"]["retention"] = json!({"max_age_seconds": 60});
    document["storage"]["backends"]["sqlite"]["retention"] = json!({"max_segments_per_cleanup": 1});
    assert!(serde_json::from_value::<StorageConfig>(document)
        .unwrap_err()
        .to_string()
        .contains("unknown field `max_segments_per_cleanup`"));
}

#[tokio::test]
async fn ndjson_inherits_the_default_age_and_limits_files_removed_per_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let config = configure(directory.path(), "ndjson", 1, 4096, None);
    let writer = open_store(config, StoreAccess::ReadWrite).await.unwrap();
    for days in [3, 2, 1] {
        let timestamp = (OffsetDateTime::now_utc() - Duration::days(days))
            .format(&Rfc3339)
            .unwrap();
        writer
            .append(request("expired", &timestamp, "a", 0))
            .await
            .unwrap();
    }
    drop(writer);
    assert_eq!(segments(directory.path()).len(), 3);

    let path = directory.path().join("storage.json");
    let mut document: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    document["storage"]["retention"] = json!({"max_age_seconds": 60});
    document["storage"]["backends"]["ndjson"]["retention"] =
        json!({"cleanup_interval_seconds": 3600, "max_segments_per_cleanup": 1});
    fs::write(&path, document.to_string()).unwrap();
    let config = StorageConfig::from_file(path).unwrap();
    let writer = open_store(config, StoreAccess::ReadWrite).await.unwrap();
    assert_eq!(segments(directory.path()).len(), 2);
    assert!(writer
        .scan(CaptureFilter::default())
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap()
        .is_empty());
}

#[test]
fn rejects_backend_settings_outside_the_selected_backend_node() {
    let missing = serde_json::from_value::<StorageConfig>(json!({
        "storage": {
            "backend": "sqlite",
            "backends": {"ndjson": {"directory": "segments"}}
        }
    }))
    .unwrap_err();
    assert!(missing
        .to_string()
        .contains("missing storage.backends.sqlite"));

    let flat = serde_json::from_value::<StorageConfig>(json!({
        "storage": {
            "backend": "sqlite",
            "path": "captures.sqlite",
            "backends": {"sqlite": {"path": "captures.sqlite"}}
        }
    }))
    .unwrap_err();
    assert!(flat.to_string().contains("unknown field `path`"));
}

#[tokio::test]
async fn both_backends_filter_a_bounded_live_snapshot() {
    for backend in ["ndjson", "sqlite"] {
        let directory = tempfile::tempdir().unwrap();
        let config = configure(directory.path(), backend, 3600, 4096, None);
        let store = open_store(config.clone(), StoreAccess::ReadWrite)
            .await
            .unwrap();
        let reader = open_store(config, StoreAccess::ReadOnly).await.unwrap();
        let records = [
            request("start", "2026-11-23T08:00:00.000000001+08:00", "a", 0),
            request("other-session", "2026-11-23T00:00:01Z", "b", 0),
            request("middle", "2026-11-23T00:00:02Z", "a", 0),
            request("end", "2026-11-23T00:00:03Z", "a", 0),
        ];
        for record in &records {
            store.append(record.clone()).await.unwrap();
        }
        let filter = CaptureFilter::parse(
            Some("a".into()),
            Some("2026-11-23"),
            Some("2026-11-23T00:00:03Z"),
        )
        .unwrap();
        let snapshot = reader.scan(filter.clone()).await.unwrap();
        let late = request("late", "2026-11-23T00:00:02.5Z", "a", 0);
        store.append(late.clone()).await.unwrap();
        assert_eq!(
            snapshot.try_collect::<Vec<_>>().await.unwrap(),
            vec![records[0].clone(), records[2].clone()]
        );
        assert_eq!(
            reader
                .scan(filter)
                .await
                .unwrap()
                .try_collect::<Vec<_>>()
                .await
                .unwrap(),
            vec![records[0].clone(), records[2].clone(), late]
        );
        assert!(matches!(
            reader.append(records[0].clone()).await,
            Err(StorageError::ReadOnly)
        ));
    }
}

#[tokio::test]
async fn ndjson_rotates_by_size_and_utc_time_without_splitting_rows() {
    let directory = tempfile::tempdir().unwrap();
    let config = configure(directory.path(), "ndjson", 3600, 420, None);
    let store = open_store(config.clone(), StoreAccess::ReadWrite)
        .await
        .unwrap();
    let records = [
        request("size-1", "2026-11-23T00:59:57Z", "a", 128),
        request("size-2", "2026-11-23T00:59:58Z", "a", 128),
        request("time", "2026-11-23T01:00:00Z", "a", 0),
        request("oversized", "2026-11-23T01:00:01Z", "a", 512),
        request("after-oversized", "2026-11-23T01:00:02Z", "a", 0),
    ];
    for record in &records {
        store.append(record.clone()).await.unwrap();
    }
    let paths = segments(directory.path());
    assert_eq!(paths.len(), 5);
    for path in &paths {
        let contents = fs::read(path).unwrap();
        assert_eq!(contents.iter().filter(|&&byte| byte == b'\n').count(), 1);
        serde_json::from_slice::<CapturedRequest>(&contents).unwrap();
    }
    drop(store);

    let last_segment = paths.last().unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(last_segment)
        .unwrap()
        .write_all(b"{\"incomplete\":")
        .unwrap();
    let reader = open_store(config.clone(), StoreAccess::ReadOnly)
        .await
        .unwrap();
    assert_eq!(
        reader
            .scan(CaptureFilter::default())
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap(),
        records
    );

    let writer = open_store(config, StoreAccess::ReadWrite).await.unwrap();
    let restarted = request("after-restart", "2026-11-23T01:00:03Z", "a", 0);
    writer.append(restarted.clone()).await.unwrap();
    let filter = CaptureFilter::parse(None, Some("2026-11-23T01:00:02Z"), None).unwrap();
    assert_eq!(
        reader
            .scan(filter)
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap(),
        vec![records[4].clone(), restarted]
    );
}

#[tokio::test]
async fn both_backends_physically_prune_expired_captures_without_new_traffic() {
    let ndjson = tempfile::tempdir().unwrap();
    let sqlite = tempfile::tempdir().unwrap();
    tokio::join!(
        verify_retention(ndjson.path(), "ndjson"),
        verify_retention(sqlite.path(), "sqlite")
    );
}

async fn verify_retention(directory: &Path, backend: &str) {
    let config = configure(directory, backend, 1, 4096, None);
    let initial = open_store(config, StoreAccess::ReadWrite).await.unwrap();
    let expired = OffsetDateTime::now_utc() - Duration::days(1);
    let expired = request("expired", &expired.format(&Rfc3339).unwrap(), "a", 0);
    initial.append(expired.clone()).await.unwrap();
    drop(initial);

    let config = configure(
        directory,
        backend,
        1,
        4096,
        Some(json!({"max_age_seconds": 1, "cleanup_interval_seconds": 1})),
    );
    let reader = open_store(config.clone(), StoreAccess::ReadOnly)
        .await
        .unwrap();
    assert!(reader
        .scan(CaptureFilter::default())
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap()
        .is_empty());
    assert_eq!(physical_count(directory, backend), 1);

    let writer = open_store(config, StoreAccess::ReadWrite).await.unwrap();
    assert_eq!(physical_count(directory, backend), 0);
    assert!(matches!(
        writer.append(expired).await,
        Err(StorageError::ExpiredCapture)
    ));
    let current = OffsetDateTime::now_utc().format(&Rfc3339).unwrap();
    writer
        .append(request("current", &current, "b", 0))
        .await
        .unwrap();
    assert_eq!(physical_count(directory, backend), 1);

    tokio::time::timeout(StdDuration::from_secs(6), async {
        while physical_count(directory, backend) != 0 {
            tokio::time::sleep(StdDuration::from_millis(100)).await;
        }
    })
    .await
    .expect("the writer should clean up expired records while idle");
}

fn request(label: &str, timestamp: &str, session: &str, padding: usize) -> CapturedRequest {
    CapturedRequest {
        captured_at: timestamp.into(),
        source: None,
        session_key: Some(session.into()),
        comparison: ComparisonMetadata {
            endpoint_key: "test".into(),
            agent_key: None,
            model_deployment_key: None,
            kv_namespace: None,
        },
        payload: json!({"label": label, "padding": "a".repeat(padding)}),
    }
}

fn configure(
    directory: &Path,
    backend: &str,
    period: u32,
    size: u64,
    retention: Option<Value>,
) -> StorageConfig {
    let mut config = match backend {
        "ndjson" => {
            json!({"directory": "segments", "segment_seconds": period, "max_segment_bytes": size})
        }
        "sqlite" => json!({"path": "captures.sqlite"}),
        _ => unreachable!(),
    };
    if let Some(retention) = retention {
        config["retention"] = retention;
    }
    let path = directory.join("storage.json");
    let document = json!({"storage": {"backend": backend, "backends": {(backend): config}}});
    fs::write(&path, document.to_string()).unwrap();
    StorageConfig::from_file(path).unwrap()
}

fn segments(directory: &Path) -> Vec<PathBuf> {
    let mut segments: Vec<_> = fs::read_dir(directory.join("segments"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "ndjson"))
        .collect();
    segments.sort();
    segments
}

fn physical_count(directory: &Path, backend: &str) -> usize {
    if backend == "ndjson" {
        return segments(directory).len();
    }
    rusqlite::Connection::open(directory.join("captures.sqlite"))
        .unwrap()
        .query_row("SELECT count(*) FROM captures", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap() as usize
}
