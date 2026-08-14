use memory_core::{MemoryRecord, MemoryStore, SqliteMemoryStore};
use rusqlite::Connection;

fn record(id: &str, text: &str, scope_id: &str, embedding: Vec<f32>) -> MemoryRecord {
    MemoryRecord {
        id: id.to_string(),
        text: text.to_string(),
        metadata: serde_json::json!({"scope_id": scope_id}),
        embedding,
        created_at_ms: 10,
        updated_at_ms: 20,
    }
}

#[tokio::test]
async fn sqlite_store_roundtrips_records() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = SqliteMemoryStore::new(temp.path().join("memory.sqlite"));
    let original = record("m1", "alpha likes coffee", "scope-a", vec![0.1, 0.2, 0.3]);

    store.add_record(&original).await.expect("add record");

    let records = store.list_records().await.expect("list records");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, original.id);
    assert_eq!(records[0].text, original.text);
    assert_eq!(records[0].metadata, original.metadata);
    assert_eq!(records[0].embedding, original.embedding);
    assert_eq!(records[0].created_at_ms, original.created_at_ms);
    assert_eq!(records[0].updated_at_ms, original.updated_at_ms);
}

#[tokio::test]
async fn sqlite_store_does_not_initialize_graph_schema() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("memory.sqlite");
    let store = SqliteMemoryStore::new(&path);

    store
        .add_record(&record("m1", "alpha likes coffee", "scope-a", vec![0.1]))
        .await
        .expect("add record");

    let connection = Connection::open(path).expect("open sqlite file");
    let graph_table_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'graph_%'",
            [],
            |row| row.get(0),
        )
        .expect("count graph tables");

    assert_eq!(graph_table_count, 0);
}

#[tokio::test]
async fn sqlite_store_replaces_existing_record_by_id() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = SqliteMemoryStore::new(temp.path().join("memory.sqlite"));

    store
        .add_record(&record("m1", "old text", "scope-a", vec![0.1]))
        .await
        .expect("add old");
    store
        .add_record(&record("m1", "new text", "scope-b", vec![0.9, 0.8]))
        .await
        .expect("replace old");

    let records = store.list_records().await.expect("list records");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, "m1");
    assert_eq!(records[0].text, "new text");
    assert_eq!(
        records[0].metadata,
        serde_json::json!({"scope_id": "scope-b"})
    );
    assert_eq!(records[0].embedding, vec![0.9, 0.8]);
}

#[tokio::test]
async fn sqlite_store_add_records_upserts_batch_without_deleting_existing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = SqliteMemoryStore::new(temp.path().join("memory.sqlite"));

    store
        .add_record(&record("m1", "old text", "scope-a", vec![0.1]))
        .await
        .expect("add old");
    store
        .add_records(&[
            record("m1", "new text", "scope-a", vec![0.9]),
            record("m2", "second text", "scope-b", vec![0.2]),
        ])
        .await
        .expect("batch upsert");

    let mut records = store.list_records().await.expect("list records");
    records.sort_by(|left, right| left.id.cmp(&right.id));

    assert_eq!(records.len(), 2);
    assert_eq!(records[0].id, "m1");
    assert_eq!(records[0].text, "new text");
    assert_eq!(records[0].embedding, vec![0.9]);
    assert_eq!(records[1].id, "m2");
    assert_eq!(records[1].text, "second text");
}

#[tokio::test]
async fn sqlite_store_replace_all_replaces_existing_records() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = SqliteMemoryStore::new(temp.path().join("memory.sqlite"));

    store
        .replace_all(&[
            record("m1", "first", "scope-a", vec![1.0]),
            record("m2", "second", "scope-a", vec![2.0]),
        ])
        .await
        .expect("replace all first");
    store
        .replace_all(&[record("m3", "third", "scope-b", vec![3.0])])
        .await
        .expect("replace all second");

    let records = store.list_records().await.expect("list records");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, "m3");
    assert_eq!(records[0].text, "third");
    assert_eq!(records[0].embedding, vec![3.0]);
}

#[tokio::test]
async fn dense_candidates_return_nearest_sqlite_vectors() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = SqliteMemoryStore::new(temp.path().join("memory.sqlite"));

    store
        .replace_all(&[
            record(
                "near",
                "near dense candidate",
                "scope-a",
                vec![0.9, 0.4358899],
            ),
            record("far", "far dense candidate", "scope-a", vec![0.0, 1.0]),
            record("other", "other scope candidate", "scope-b", vec![1.0, 0.0]),
        ])
        .await
        .expect("replace all");

    let results = store
        .dense_candidates(
            &[1.0, 0.0],
            Some(&serde_json::json!({"scope_id": "scope-a"})),
            2,
        )
        .await
        .expect("dense candidates");

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].record.id, "near");
    assert_eq!(results[1].record.id, "far");
    assert!(results[0].score > results[1].score);
}

#[tokio::test]
async fn bm25_candidates_return_keyword_matches() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = SqliteMemoryStore::new(temp.path().join("memory.sqlite"));

    store
        .replace_all(&[
            record(
                "m1",
                "User loves Pacific Islander melodies and remix albums.",
                "scope-a",
                vec![0.1],
            ),
            record("m2", "User bought new running shoes.", "scope-a", vec![0.2]),
        ])
        .await
        .expect("replace all");

    let results = store
        .bm25_candidates("Pacific melodies", None, 5)
        .await
        .expect("bm25 candidates");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].record.id, "m1");
    assert!(results[0].score > 0.0);
}

#[tokio::test]
async fn bm25_candidates_honor_scope_id_filter() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = SqliteMemoryStore::new(temp.path().join("memory.sqlite"));

    store
        .replace_all(&[
            record("m1", "User likes Pacific melodies.", "scope-a", vec![0.1]),
            record("m2", "User studies Pacific melodies.", "scope-b", vec![0.2]),
        ])
        .await
        .expect("replace all");

    let results = store
        .bm25_candidates(
            "Pacific melodies",
            Some(&serde_json::json!({"scope_id": "scope-b"})),
            5,
        )
        .await
        .expect("bm25 candidates");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].record.id, "m2");
}

#[tokio::test]
async fn bm25_candidates_update_when_record_is_replaced() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = SqliteMemoryStore::new(temp.path().join("memory.sqlite"));

    store
        .add_record(&record("m1", "obsolete lemon note", "scope-a", vec![0.1]))
        .await
        .expect("add old");
    store
        .add_record(&record("m1", "fresh coffee note", "scope-a", vec![0.2]))
        .await
        .expect("replace old");

    let old_results = store
        .bm25_candidates("obsolete", None, 5)
        .await
        .expect("old bm25 candidates");
    let fresh_results = store
        .bm25_candidates("fresh coffee", None, 5)
        .await
        .expect("fresh bm25 candidates");

    assert!(old_results.is_empty());
    assert_eq!(fresh_results.len(), 1);
    assert_eq!(fresh_results[0].record.id, "m1");
}
