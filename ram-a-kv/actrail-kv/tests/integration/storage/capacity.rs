//! Verifies capacity eviction, oversized-write rejection and migration using real storage backends.
use actrail_kv_artifacts::{CapturedRequest, ComparisonMetadata};
use actrail_kv_storage::{
    open_store, CaptureFilter, StorageBackendKind, StorageConfig, StorageError, StoreAccess,
};
use futures_util::TryStreamExt;
use serde_json::json;
use std::{num::NonZeroU64, path::Path};

fn record(index: u8, padding: usize) -> CapturedRequest {
    CapturedRequest {
        captured_at: format!("2026-11-26T00:00:0{index}Z"),
        source: None,
        session_key: Some("capacity".into()),
        comparison: ComparisonMetadata {
            endpoint_key: "test".into(),
            agent_key: None,
            model_deployment_key: None,
            kv_namespace: None,
        },
        payload: json!({"index": index, "text": "数据".repeat(padding)}),
    }
}

fn config(directory: &Path, backend: &str, limit: u64) -> StorageConfig {
    let mut config = StorageConfig::default();
    config.retention.as_mut().unwrap().max_age_seconds = 0;
    config.retention.as_mut().unwrap().max_size_bytes = NonZeroU64::new(limit).unwrap();
    config.backend = match backend {
        "sqlite" => StorageBackendKind::Sqlite,
        "ndjson" => StorageBackendKind::Ndjson,
        _ => unreachable!(),
    };
    config.backends.sqlite.as_mut().unwrap().path = directory.join("captures.sqlite");
    config.backends.ndjson.as_mut().unwrap().directory = directory.join("segments");
    config
}

fn bytes(directory: &Path, backend: &str) -> u64 {
    if backend == "sqlite" {
        let connection = rusqlite::Connection::open(directory.join("captures.sqlite")).unwrap();
        let total: i64 = connection
            .query_row(
                "SELECT COALESCE(SUM(length(CAST(record AS BLOB))), 0) FROM captures",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let counter: i64 = connection
            .query_row(
                "SELECT total_bytes FROM capture_usage WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(counter, total);
        total as u64
    } else {
        std::fs::read_dir(directory.join("segments"))
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "ndjson"))
            .map(|entry| entry.metadata().unwrap().len())
            .sum()
    }
}

#[tokio::test]
async fn capacity_evicts_oldest_and_survives_reopen_with_time_cleanup_disabled() {
    for backend in ["sqlite", "ndjson"] {
        let directory = tempfile::tempdir().unwrap();
        let records: Vec<_> = (0..4).map(|index| record(index, 20)).collect();
        let size = serde_json::to_vec(&records[0]).unwrap().len() as u64 + 1;
        let mut settings = config(directory.path(), backend, 2 * size);
        // One record per NDJSON segment allows both backends to retain the same records.
        settings.backends.ndjson.as_mut().unwrap().max_segment_bytes = NonZeroU64::new(1).unwrap();
        settings.backends.ndjson.as_mut().unwrap().segment_seconds =
            std::num::NonZeroU32::new(1).unwrap();
        let store = open_store(settings.clone(), StoreAccess::ReadWrite)
            .await
            .unwrap();
        for index in [2, 0, 1] {
            store.append(records[index].clone()).await.unwrap();
        }
        let retained = store
            .scan(CaptureFilter::default())
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert_eq!(retained, records[1..3]);
        assert!(bytes(directory.path(), backend) <= 2 * size);

        let before = bytes(directory.path(), backend);
        assert!(matches!(
            store.append(record(4, size as usize)).await,
            Err(StorageError::RecordTooLarge { .. })
        ));
        assert_eq!(bytes(directory.path(), backend), before);
        assert_eq!(
            store
                .scan(CaptureFilter::default())
                .await
                .unwrap()
                .try_collect::<Vec<_>>()
                .await
                .unwrap(),
            retained
        );
        drop(store);

        settings.retention.as_mut().unwrap().max_size_bytes = NonZeroU64::new(size).unwrap();
        let store = open_store(settings.clone(), StoreAccess::ReadWrite)
            .await
            .unwrap();
        assert_eq!(
            store
                .scan(CaptureFilter::default())
                .await
                .unwrap()
                .try_collect::<Vec<_>>()
                .await
                .unwrap(),
            vec![records[2].clone()]
        );
        store.append(records[3].clone()).await.unwrap();
        drop(store);
        let store = open_store(settings, StoreAccess::ReadWrite).await.unwrap();
        assert_eq!(
            store
                .scan(CaptureFilter::default())
                .await
                .unwrap()
                .try_collect::<Vec<_>>()
                .await
                .unwrap(),
            vec![records[3].clone()]
        );
        assert!(bytes(directory.path(), backend) <= size);
    }
}

#[tokio::test]
async fn backend_capacity_overrides_shared_limit_and_replaces_current_segment() {
    for backend in ["sqlite", "ndjson"] {
        let directory = tempfile::tempdir().unwrap();
        let size = serde_json::to_vec(&record(0, 20)).unwrap().len() as u64 + 1;
        let mut settings = config(directory.path(), backend, size * 10);
        settings
            .backends
            .sqlite
            .as_mut()
            .unwrap()
            .retention
            .as_mut()
            .unwrap()
            .max_size_bytes = NonZeroU64::new(size);
        settings
            .backends
            .ndjson
            .as_mut()
            .unwrap()
            .retention
            .as_mut()
            .unwrap()
            .max_size_bytes = NonZeroU64::new(size);
        let store = open_store(settings, StoreAccess::ReadWrite).await.unwrap();
        for index in 0..4 {
            // NDJSON keeps its normal time window, so each eviction deletes the cached current segment.
            store.append(record(index, 20)).await.unwrap();
            assert_eq!(
                store
                    .scan(CaptureFilter::default())
                    .await
                    .unwrap()
                    .try_collect::<Vec<_>>()
                    .await
                    .unwrap(),
                vec![record(index, 20)]
            );
            assert!(bytes(directory.path(), backend) <= size);
        }
    }
}

#[tokio::test]
async fn sqlite_backfills_existing_records_and_keeps_counter_consistent_after_deletion() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("captures.sqlite");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch("CREATE TABLE captures (id INTEGER PRIMARY KEY, session_key TEXT, captured_at TEXT NOT NULL, record TEXT NOT NULL)").unwrap();
    for index in 0..3 {
        let record = record(index, 20);
        connection
            .execute(
                "INSERT INTO captures(session_key, captured_at, record) VALUES (?, ?, ?)",
                rusqlite::params![
                    record.session_key,
                    record.captured_at,
                    serde_json::to_string(&record).unwrap()
                ],
            )
            .unwrap();
    }
    drop(connection);
    let size = serde_json::to_vec(&record(0, 20)).unwrap().len() as u64;
    let store = open_store(
        config(directory.path(), "sqlite", size),
        StoreAccess::ReadWrite,
    )
    .await
    .unwrap();
    assert_eq!(bytes(directory.path(), "sqlite"), size);
    assert_eq!(
        store
            .scan(CaptureFilter::default())
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap(),
        vec![record(2, 20)]
    );
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute("DELETE FROM captures", []).unwrap();
    assert_eq!(bytes(directory.path(), "sqlite"), 0);
    store.append(record(3, 20)).await.unwrap();
    assert_eq!(bytes(directory.path(), "sqlite"), size);
    // An insertion failure must roll back the preceding eviction and its counter changes.
    connection
        .execute_batch(
            "CREATE TRIGGER reject_capture BEFORE INSERT ON captures
        WHEN NEW.session_key = 'blocked!' BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
        )
        .unwrap();
    let mut rejected = record(4, 20);
    rejected.session_key = Some("blocked!".into());
    assert!(store.append(rejected).await.is_err());
    assert_eq!(bytes(directory.path(), "sqlite"), size);
    assert_eq!(
        store
            .scan(CaptureFilter::default())
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap(),
        vec![record(3, 20)]
    );
}
