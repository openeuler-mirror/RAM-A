use memory_core::sqlite::initialize_schema;
use rusqlite::Connection;

#[test]
fn graph_schema_creates_required_tables_and_indexes() {
    let connection = Connection::open_in_memory().unwrap();
    initialize_schema(&connection).unwrap();

    let mut statement = connection
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type IN ('table', 'index')
             ORDER BY name",
        )
        .unwrap();
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    for required in [
        "graph_memory_spaces",
        "graph_memory_records",
        "graph_memory_record_fts",
        "graph_entities",
        "graph_entity_aliases",
        "graph_facts",
        "graph_fact_evidence_groups",
        "graph_fact_evidence",
        "graph_fact_links",
        "graph_fact_link_evidence_groups",
        "graph_fact_link_evidence",
        "graph_ingestion_runs",
        "graph_extraction_runs",
        "graph_resolution_decisions",
        "graph_fact_status_history",
        "graph_fact_fts",
        "graph_entity_fts",
        "graph_entity_alias_fts",
    ] {
        assert!(
            names.iter().any(|name| name == required),
            "missing {required}"
        );
    }
}

#[test]
fn graph_schema_enforces_foreign_keys_and_memory_space_scope() {
    let connection = Connection::open_in_memory().unwrap();
    initialize_schema(&connection).unwrap();

    let enabled: i64 = connection
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();
    assert_eq!(enabled, 1);

    let result = connection.execute(
        "INSERT INTO graph_facts (
            id, memory_space_id, subject_entity_id, predicate, object_entity_id,
            fact_text, status, recorded_at_ms, type_registry_version
         ) VALUES ('f1', 'space-a', 'missing-a', 'LIVES_IN', 'missing-b',
                   'Alice lives in Shanghai.', 'active', 1, 'graph-type-registry-v1')",
        [],
    );

    assert!(result.is_err());
}

#[test]
fn graph_schema_links_run_scoped_tables_to_ingestion_runs() {
    let connection = Connection::open_in_memory().unwrap();
    initialize_schema(&connection).unwrap();

    let extraction_foreign_key_columns =
        foreign_key_columns_to_table(&connection, "graph_extraction_runs", "graph_ingestion_runs");
    assert!(
        extraction_foreign_key_columns
            .contains(&("ingestion_run_id".to_string(), "id".to_string())),
        "graph_extraction_runs must reference graph_ingestion_runs.id"
    );
    assert!(
        extraction_foreign_key_columns
            .contains(&("memory_space_id".to_string(), "memory_space_id".to_string())),
        "graph_extraction_runs must reference graph_ingestion_runs.memory_space_id"
    );

    let extraction_foreign_keys = foreign_key_targets(&connection, "graph_extraction_runs");
    assert!(
        extraction_foreign_keys
            .iter()
            .any(|target| target == "graph_ingestion_runs"),
        "graph_extraction_runs must reference graph_ingestion_runs"
    );

    let resolution_foreign_keys = foreign_key_targets(&connection, "graph_resolution_decisions");
    assert!(
        resolution_foreign_keys
            .iter()
            .any(|target| target == "graph_ingestion_runs"),
        "graph_resolution_decisions must reference graph_ingestion_runs"
    );
}

fn foreign_key_columns_to_table(
    connection: &Connection,
    table_name: &str,
    target_table: &str,
) -> Vec<(String, String)> {
    let mut statement = connection
        .prepare(&format!("PRAGMA foreign_key_list({table_name})"))
        .unwrap();
    statement
        .query_map([], |row| {
            let table: String = row.get(2)?;
            let from: String = row.get(3)?;
            let to: String = row.get(4)?;
            Ok((table, from, to))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .into_iter()
        .filter_map(|(table, from, to)| (table == target_table).then_some((from, to)))
        .collect()
}

fn foreign_key_targets(connection: &Connection, table_name: &str) -> Vec<String> {
    let mut statement = connection
        .prepare(&format!("PRAGMA foreign_key_list({table_name})"))
        .unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(2))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}
