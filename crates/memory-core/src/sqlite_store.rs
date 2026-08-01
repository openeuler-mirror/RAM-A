use std::{
    any::Any,
    ffi::{c_char, c_int},
    path::{Path, PathBuf},
    sync::Once,
    thread,
    time::Duration,
};

use async_trait::async_trait;
use rusqlite::{ffi::sqlite3_auto_extension, params, Connection, ErrorCode, TransactionBehavior};
use sqlite_vec::sqlite3_vec_init;

use crate::{
    record::{extract_scope_id, extract_scope_id_from_filter, metadata_matches},
    MemoryError, MemoryRecord, MemoryResult, MemoryStore, ScoredMemory,
};

static REGISTER_SQLITE_VEC: Once = Once::new();

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(30);
const SQLITE_LOCK_RETRY_ATTEMPTS: usize = 3;
const SQLITE_LOCK_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(50);
const SQLITE_LOCK_RETRY_MAX_DELAY: Duration = Duration::from_millis(500);

type SqliteAutoExtensionEntry = unsafe extern "C" fn(
    *mut rusqlite::ffi::sqlite3,
    *mut *mut c_char,
    *const rusqlite::ffi::sqlite3_api_routines,
) -> c_int;

pub struct SqliteMemoryStore {
    path: PathBuf,
}

impl SqliteMemoryStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn initialize(&self) -> MemoryResult<()> {
        let path = self.path.clone();
        run_sqlite_operation(move || {
            let _connection = open_connection(&path)?;
            Ok(())
        })
        .await
    }

    pub async fn bm25_candidates(
        &self,
        query: &str,
        filter: Option<&serde_json::Value>,
        limit: usize,
    ) -> MemoryResult<Vec<ScoredMemory>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let query = sanitize_fts_query(query);
        if query.is_empty() {
            return Ok(Vec::new());
        }

        let path = self.path.clone();
        let filter = filter.cloned();
        run_sqlite_operation(move || {
            let connection = open_connection(&path)?;
            bm25_candidates(&connection, &query, filter.as_ref(), limit)
        })
        .await
    }

    pub async fn dense_candidates(
        &self,
        query_embedding: &[f32],
        filter: Option<&serde_json::Value>,
        limit: usize,
    ) -> MemoryResult<Vec<ScoredMemory>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let path = self.path.clone();
        let query_embedding = query_embedding.to_vec();
        let filter = filter.cloned();
        run_sqlite_operation(move || {
            let connection = open_connection(&path)?;
            dense_candidates(&connection, &query_embedding, filter.as_ref(), limit)
        })
        .await
    }
}

#[async_trait]
impl MemoryStore for SqliteMemoryStore {
    fn as_any(&self) -> &dyn Any {
        self
    }

    async fn add_record(&self, record: &MemoryRecord) -> MemoryResult<()> {
        let path = self.path.clone();
        let record = record.clone();
        run_sqlite_operation(move || {
            let mut connection = open_connection(&path)?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            upsert_record(&transaction, &record)?;
            transaction.commit()?;
            Ok(())
        })
        .await
    }

    async fn add_records(&self, records: &[MemoryRecord]) -> MemoryResult<()> {
        let path = self.path.clone();
        let records = records.to_vec();
        run_sqlite_operation(move || {
            let mut connection = open_connection(&path)?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            for record in &records {
                upsert_record(&transaction, record)?;
            }
            transaction.commit()?;
            Ok(())
        })
        .await
    }

    async fn list_records(&self) -> MemoryResult<Vec<MemoryRecord>> {
        let path = self.path.clone();
        run_sqlite_operation(move || {
            let connection = open_connection(&path)?;
            list_records(&connection)
        })
        .await
    }

    async fn replace_all(&self, records: &[MemoryRecord]) -> MemoryResult<()> {
        let path = self.path.clone();
        let records = records.to_vec();
        run_sqlite_operation(move || {
            let mut connection = open_connection(&path)?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute("DELETE FROM memories", [])?;
            transaction.execute("DELETE FROM memory_fts", [])?;
            for record in &records {
                upsert_record(&transaction, record)?;
            }
            transaction.commit()?;
            Ok(())
        })
        .await
    }
}

async fn run_sqlite_operation<T, F>(operation: F) -> MemoryResult<T>
where
    T: Send + 'static,
    F: FnMut() -> MemoryResult<T> + Send + 'static,
{
    tokio::task::spawn_blocking(move || retry_sqlite_locked(operation))
        .await
        .map_err(|error| MemoryError::StoreBackend {
            message: format!("sqlite task failed: {error}"),
        })?
}

fn open_connection(path: &Path) -> MemoryResult<Connection> {
    register_sqlite_vec_extension();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let connection = Connection::open(path)?;
    configure_connection(&connection)?;
    initialize_schema(&connection)?;
    Ok(connection)
}

fn configure_connection(connection: &Connection) -> MemoryResult<()> {
    connection.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

fn retry_sqlite_locked<T, F>(mut operation: F) -> MemoryResult<T>
where
    F: FnMut() -> MemoryResult<T>,
{
    let mut delay = SQLITE_LOCK_RETRY_INITIAL_DELAY;
    for attempt in 0..=SQLITE_LOCK_RETRY_ATTEMPTS {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) => {
                if !is_retryable_sqlite_lock(&error) || attempt == SQLITE_LOCK_RETRY_ATTEMPTS {
                    return Err(error);
                }
                thread::sleep(delay);
                let next_delay_ms = delay
                    .as_millis()
                    .saturating_mul(2)
                    .min(SQLITE_LOCK_RETRY_MAX_DELAY.as_millis());
                delay = Duration::from_millis(next_delay_ms as u64);
            }
        }
    }
    unreachable!("sqlite retry loop always returns");
}

fn is_retryable_sqlite_lock(error: &MemoryError) -> bool {
    match error {
        MemoryError::Sqlite(sqlite_error) => matches!(
            sqlite_error.sqlite_error_code(),
            Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
        ),
        _ => false,
    }
}

fn register_sqlite_vec_extension() {
    REGISTER_SQLITE_VEC.call_once(|| unsafe {
        sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            SqliteAutoExtensionEntry,
        >(sqlite3_vec_init as *const ())));
    });
}

fn initialize_schema(connection: &Connection) -> MemoryResult<()> {
    connection.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS memories (
            id TEXT PRIMARY KEY,
            text TEXT NOT NULL,
            metadata_json TEXT NOT NULL,
            embedding BLOB NOT NULL,
            embedding_dims INTEGER NOT NULL,
            scope_id TEXT,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_memories_scope_id
        ON memories(scope_id);

        CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts
        USING fts5(id UNINDEXED, text);
        "#,
    )?;
    Ok(())
}

fn upsert_record(connection: &Connection, record: &MemoryRecord) -> MemoryResult<()> {
    let metadata_json = serde_json::to_string(&record.metadata)?;
    let embedding = embedding_to_blob(&record.embedding);
    let scope_id = extract_scope_id(&record.metadata);
    connection.execute(
        r#"
        INSERT INTO memories (
            id,
            text,
            metadata_json,
            embedding,
            embedding_dims,
            scope_id,
            created_at_ms,
            updated_at_ms
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        ON CONFLICT(id) DO UPDATE SET
            text = excluded.text,
            metadata_json = excluded.metadata_json,
            embedding = excluded.embedding,
            embedding_dims = excluded.embedding_dims,
            scope_id = excluded.scope_id,
            updated_at_ms = excluded.updated_at_ms
        "#,
        params![
            record.id,
            record.text,
            metadata_json,
            embedding,
            record.embedding.len() as i64,
            scope_id,
            record.created_at_ms as i64,
            record.updated_at_ms as i64,
        ],
    )?;
    connection.execute("DELETE FROM memory_fts WHERE id = ?1", params![record.id])?;
    connection.execute(
        "INSERT INTO memory_fts(id, text) VALUES (?1, ?2)",
        params![record.id, record.text],
    )?;
    Ok(())
}

fn list_records(connection: &Connection) -> MemoryResult<Vec<MemoryRecord>> {
    let mut statement = connection.prepare(
        r#"
        SELECT
            id,
            text,
            metadata_json,
            embedding,
            embedding_dims,
            created_at_ms,
            updated_at_ms
        FROM memories
        ORDER BY rowid ASC
        "#,
    )?;
    let rows = statement.query_map([], |row| {
        let id: String = row.get(0)?;
        let text: String = row.get(1)?;
        let metadata_json: String = row.get(2)?;
        let embedding_blob: Vec<u8> = row.get(3)?;
        let embedding_dims: i64 = row.get(4)?;
        let created_at_ms: i64 = row.get(5)?;
        let updated_at_ms: i64 = row.get(6)?;
        Ok((
            id,
            text,
            metadata_json,
            embedding_blob,
            embedding_dims,
            created_at_ms,
            updated_at_ms,
        ))
    })?;

    let mut records = Vec::new();
    for row in rows {
        let (id, text, metadata_json, embedding_blob, embedding_dims, created_at_ms, updated_at_ms) =
            row?;
        let metadata = serde_json::from_str(&metadata_json)?;
        let embedding = blob_to_embedding(&embedding_blob)?;
        if embedding.len() != embedding_dims as usize {
            return Err(MemoryError::StoreBackend {
                message: format!(
                    "embedding dims mismatch for record '{id}': blob has {} dims but stored dims is {embedding_dims}",
                    embedding.len()
                ),
            });
        }
        records.push(MemoryRecord {
            id,
            text,
            metadata,
            embedding,
            created_at_ms: created_at_ms as u64,
            updated_at_ms: updated_at_ms as u64,
        });
    }
    Ok(records)
}

fn bm25_candidates(
    connection: &Connection,
    query: &str,
    filter: Option<&serde_json::Value>,
    limit: usize,
) -> MemoryResult<Vec<ScoredMemory>> {
    let scope_id = extract_scope_id_from_filter(filter);
    let mut candidates = if let Some(scope_id) = scope_id.as_deref() {
        let mut statement = connection.prepare(
            r#"
            SELECT
                memories.id,
                memories.text,
                memories.metadata_json,
                memories.embedding,
                memories.embedding_dims,
                memories.created_at_ms,
                memories.updated_at_ms,
                bm25(memory_fts) AS bm25_raw
            FROM memory_fts
            JOIN memories ON memories.id = memory_fts.id
            WHERE memory_fts MATCH ?1
              AND memories.scope_id = ?2
            ORDER BY bm25(memory_fts)
            LIMIT ?3
            "#,
        )?;
        collect_bm25_rows(&mut statement, params![query, scope_id, limit as i64])?
    } else {
        let mut statement = connection.prepare(
            r#"
            SELECT
                memories.id,
                memories.text,
                memories.metadata_json,
                memories.embedding,
                memories.embedding_dims,
                memories.created_at_ms,
                memories.updated_at_ms,
                bm25(memory_fts) AS bm25_raw
            FROM memory_fts
            JOIN memories ON memories.id = memory_fts.id
            WHERE memory_fts MATCH ?1
            ORDER BY bm25(memory_fts)
            LIMIT ?2
            "#,
        )?;
        collect_bm25_rows(&mut statement, params![query, limit as i64])?
    };

    candidates.retain(|candidate| metadata_matches(&candidate.record.metadata, filter));
    normalize_lower_is_better_scores(&mut candidates);
    candidates.truncate(limit);
    Ok(candidates)
}

fn dense_candidates(
    connection: &Connection,
    query_embedding: &[f32],
    filter: Option<&serde_json::Value>,
    limit: usize,
) -> MemoryResult<Vec<ScoredMemory>> {
    if query_embedding.is_empty() {
        return Err(MemoryError::Embedding {
            message: "query embedding must not be empty".to_string(),
        });
    }

    let query_dims = query_embedding.len();
    ensure_no_dense_dimension_mismatch(connection, query_dims, filter)?;
    let query_embedding = embedding_to_blob(query_embedding);
    let scope_id = extract_scope_id_from_filter(filter);
    let mut candidates = if let Some(scope_id) = scope_id.as_deref() {
        let mut statement = connection.prepare(
            r#"
            SELECT
                id,
                text,
                metadata_json,
                embedding,
                embedding_dims,
                created_at_ms,
                updated_at_ms,
                vec_distance_cosine(vec_f32(embedding), vec_f32(?1)) AS distance
            FROM memories
            WHERE embedding_dims = ?2
              AND scope_id = ?3
            ORDER BY distance ASC, id ASC
            LIMIT ?4
            "#,
        )?;
        collect_dense_rows(
            &mut statement,
            params![query_embedding, query_dims as i64, scope_id, limit as i64],
        )?
    } else {
        let mut statement = connection.prepare(
            r#"
            SELECT
                id,
                text,
                metadata_json,
                embedding,
                embedding_dims,
                created_at_ms,
                updated_at_ms,
                vec_distance_cosine(vec_f32(embedding), vec_f32(?1)) AS distance
            FROM memories
            WHERE embedding_dims = ?2
            ORDER BY distance ASC, id ASC
            LIMIT ?3
            "#,
        )?;
        collect_dense_rows(
            &mut statement,
            params![query_embedding, query_dims as i64, limit as i64],
        )?
    };

    candidates.retain(|candidate| metadata_matches(&candidate.record.metadata, filter));
    candidates.truncate(limit);
    Ok(candidates)
}

fn ensure_no_dense_dimension_mismatch(
    connection: &Connection,
    query_dims: usize,
    filter: Option<&serde_json::Value>,
) -> MemoryResult<()> {
    let scope_id = extract_scope_id_from_filter(filter);
    let mismatch_count: i64 = if let Some(scope_id) = scope_id.as_deref() {
        connection.query_row(
            "SELECT COUNT(*) FROM memories WHERE embedding_dims != ?1 AND scope_id = ?2",
            params![query_dims as i64, scope_id],
            |row| row.get(0),
        )?
    } else {
        connection.query_row(
            "SELECT COUNT(*) FROM memories WHERE embedding_dims != ?1",
            params![query_dims as i64],
            |row| row.get(0),
        )?
    };

    if mismatch_count > 0 {
        return Err(MemoryError::Embedding {
            message: format!(
                "embedding dimension mismatch: query has {query_dims} dims but {mismatch_count} stored records have different dimensions"
            ),
        });
    }
    Ok(())
}

fn collect_dense_rows<P>(
    statement: &mut rusqlite::Statement<'_>,
    params: P,
) -> MemoryResult<Vec<ScoredMemory>>
where
    P: rusqlite::Params,
{
    let rows = statement.query_map(params, |row| {
        let id: String = row.get(0)?;
        let text: String = row.get(1)?;
        let metadata_json: String = row.get(2)?;
        let embedding_blob: Vec<u8> = row.get(3)?;
        let embedding_dims: i64 = row.get(4)?;
        let created_at_ms: i64 = row.get(5)?;
        let updated_at_ms: i64 = row.get(6)?;
        let distance: f32 = row.get(7)?;
        Ok((
            id,
            text,
            metadata_json,
            embedding_blob,
            embedding_dims,
            created_at_ms,
            updated_at_ms,
            distance,
        ))
    })?;

    let mut candidates = Vec::new();
    for row in rows {
        let (
            id,
            text,
            metadata_json,
            embedding_blob,
            embedding_dims,
            created_at_ms,
            updated_at_ms,
            distance,
        ) = row?;
        let metadata = serde_json::from_str(&metadata_json)?;
        let embedding = blob_to_embedding(&embedding_blob)?;
        if embedding.len() != embedding_dims as usize {
            return Err(MemoryError::StoreBackend {
                message: format!(
                    "embedding dims mismatch for record '{id}': blob has {} dims but stored dims is {embedding_dims}",
                    embedding.len()
                ),
            });
        }
        candidates.push(ScoredMemory {
            record: MemoryRecord {
                id,
                text,
                metadata,
                embedding,
                created_at_ms: created_at_ms as u64,
                updated_at_ms: updated_at_ms as u64,
            },
            score: 1.0 - distance,
        });
    }
    Ok(candidates)
}

fn collect_bm25_rows<P>(
    statement: &mut rusqlite::Statement<'_>,
    params: P,
) -> MemoryResult<Vec<ScoredMemory>>
where
    P: rusqlite::Params,
{
    let rows = statement.query_map(params, |row| {
        let id: String = row.get(0)?;
        let text: String = row.get(1)?;
        let metadata_json: String = row.get(2)?;
        let embedding_blob: Vec<u8> = row.get(3)?;
        let embedding_dims: i64 = row.get(4)?;
        let created_at_ms: i64 = row.get(5)?;
        let updated_at_ms: i64 = row.get(6)?;
        let bm25_raw: f32 = row.get(7)?;
        Ok((
            id,
            text,
            metadata_json,
            embedding_blob,
            embedding_dims,
            created_at_ms,
            updated_at_ms,
            bm25_raw,
        ))
    })?;

    let mut candidates = Vec::new();
    for row in rows {
        let (
            id,
            text,
            metadata_json,
            embedding_blob,
            embedding_dims,
            created_at_ms,
            updated_at_ms,
            bm25_raw,
        ) = row?;
        let metadata = serde_json::from_str(&metadata_json)?;
        let embedding = blob_to_embedding(&embedding_blob)?;
        if embedding.len() != embedding_dims as usize {
            return Err(MemoryError::StoreBackend {
                message: format!(
                    "embedding dims mismatch for record '{id}': blob has {} dims but stored dims is {embedding_dims}",
                    embedding.len()
                ),
            });
        }
        candidates.push(ScoredMemory {
            record: MemoryRecord {
                id,
                text,
                metadata,
                embedding,
                created_at_ms: created_at_ms as u64,
                updated_at_ms: updated_at_ms as u64,
            },
            score: bm25_raw,
        });
    }
    Ok(candidates)
}

fn sanitize_fts_query(query: &str) -> String {
    query
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_lower_is_better_scores(candidates: &mut [ScoredMemory]) {
    if candidates.is_empty() {
        return;
    }

    let mut min_score = f32::INFINITY;
    let mut max_score = f32::NEG_INFINITY;
    for candidate in candidates.iter() {
        min_score = min_score.min(candidate.score);
        max_score = max_score.max(candidate.score);
    }

    let range = max_score - min_score;
    if range.abs() <= f32::EPSILON {
        for candidate in candidates {
            candidate.score = 1.0;
        }
        return;
    }

    for candidate in candidates {
        candidate.score = (max_score - candidate.score) / range;
    }
}

fn embedding_to_blob(embedding: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(embedding.len() * 4);
    for value in embedding {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn blob_to_embedding(bytes: &[u8]) -> MemoryResult<Vec<f32>> {
    if !bytes.len().is_multiple_of(4) {
        return Err(MemoryError::StoreBackend {
            message: format!(
                "invalid embedding blob length {}, expected a multiple of 4",
                bytes.len()
            ),
        });
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}
