import sqlite3

from graph_audit import build_audit


def create_graph_schema(connection):
    connection.executescript(
        """
        CREATE TABLE graph_memory_spaces (id TEXT PRIMARY KEY);
        CREATE TABLE graph_memory_records (
            id TEXT PRIMARY KEY,
            memory_space_id TEXT NOT NULL,
            deleted_at_ms INTEGER
        );
        CREATE TABLE graph_entities (
            id TEXT PRIMARY KEY,
            memory_space_id TEXT NOT NULL,
            entity_type TEXT NOT NULL,
            status TEXT NOT NULL,
            deleted_at_ms INTEGER
        );
        CREATE TABLE graph_entity_aliases (
            id TEXT PRIMARY KEY,
            memory_space_id TEXT NOT NULL,
            deleted_at_ms INTEGER
        );
        CREATE TABLE graph_facts (
            id TEXT PRIMARY KEY,
            memory_space_id TEXT NOT NULL,
            predicate TEXT NOT NULL,
            status TEXT NOT NULL,
            retired_at_ms INTEGER
        );
        CREATE TABLE graph_fact_evidence_groups (
            id TEXT PRIMARY KEY,
            memory_space_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            deleted_at_ms INTEGER
        );
        CREATE TABLE graph_fact_evidence (
            id TEXT PRIMARY KEY,
            memory_space_id TEXT NOT NULL,
            evidence_group_id TEXT NOT NULL,
            memory_record_id TEXT NOT NULL,
            deleted_at_ms INTEGER
        );
        CREATE TABLE graph_ingestion_runs (
            id TEXT PRIMARY KEY,
            memory_space_id TEXT NOT NULL,
            status TEXT NOT NULL,
            stage TEXT NOT NULL
        );
        CREATE TABLE graph_extraction_runs (
            id TEXT PRIMARY KEY,
            memory_space_id TEXT NOT NULL,
            status TEXT NOT NULL
        );
        """
    )


def test_build_audit_reports_source_coverage_and_statuses():
    connection = sqlite3.connect(":memory:")
    create_graph_schema(connection)
    connection.executescript(
        """
        INSERT INTO graph_memory_spaces VALUES ('space-a');
        INSERT INTO graph_memory_records VALUES ('record-1', 'space-a', NULL);
        INSERT INTO graph_memory_records VALUES ('record-2', 'space-a', NULL);
        INSERT INTO graph_memory_records VALUES ('record-deleted', 'space-a', 1);
        INSERT INTO graph_entities VALUES ('entity-1', 'space-a', 'PERSON', 'active', NULL);
        INSERT INTO graph_entities VALUES ('entity-deleted', 'space-a', 'PERSON', 'active', 1);
        INSERT INTO graph_entity_aliases VALUES ('alias-1', 'space-a', NULL);
        INSERT INTO graph_facts VALUES ('fact-1', 'space-a', 'LIKES', 'active', NULL);
        INSERT INTO graph_facts VALUES ('fact-retired', 'space-a', 'LIKES', 'active', 1);
        INSERT INTO graph_fact_evidence_groups VALUES ('group-1', 'space-a', 'fact-1', NULL);
        INSERT INTO graph_fact_evidence VALUES ('evidence-1', 'space-a', 'group-1', 'record-1', NULL);
        INSERT INTO graph_ingestion_runs VALUES ('run-1', 'space-a', 'completed', 'completed');
        INSERT INTO graph_ingestion_runs VALUES ('run-2', 'space-a', 'failed', 'extracting');
        INSERT INTO graph_extraction_runs VALUES ('extract-1', 'space-a', 'completed');
        """
    )

    audit = build_audit(connection, "space-a")

    assert audit["summary"] == {
        "memory_spaces": 1,
        "records": 2,
        "active_entities": 1,
        "aliases": 1,
        "active_facts": 1,
        "evidence_links": 1,
        "facts_with_evidence": 1,
        "facts_without_evidence": 0,
        "records_with_fact_evidence": 1,
        "records_without_fact_evidence": 1,
        "record_fact_evidence_coverage": 0.5,
    }
    assert audit["predicate_distribution"] == [{"predicate": "LIKES", "count": 1}]
    assert audit["entity_type_distribution"] == [{"entity_type": "PERSON", "count": 1}]
    assert audit["ingestion_status_stage_distribution"] == [
        {"status": "completed", "stage": "completed", "count": 1},
        {"status": "failed", "stage": "extracting", "count": 1},
    ]
    assert audit["extraction_status_distribution"] == [{"status": "completed", "count": 1}]


def test_build_audit_scopes_all_counts_to_requested_memory_space():
    connection = sqlite3.connect(":memory:")
    create_graph_schema(connection)
    connection.executescript(
        """
        INSERT INTO graph_memory_spaces VALUES ('space-a');
        INSERT INTO graph_memory_spaces VALUES ('space-b');
        INSERT INTO graph_memory_records VALUES ('record-a', 'space-a', NULL);
        INSERT INTO graph_memory_records VALUES ('record-b', 'space-b', NULL);
        INSERT INTO graph_facts VALUES ('fact-a', 'space-a', 'LIKES', 'active', NULL);
        INSERT INTO graph_facts VALUES ('fact-b', 'space-b', 'VISITED', 'active', NULL);
        """
    )

    audit = build_audit(connection, "space-b")

    assert audit["memory_space_id"] == "space-b"
    assert audit["summary"]["records"] == 1
    assert audit["summary"]["active_facts"] == 1
    assert audit["predicate_distribution"] == [{"predicate": "VISITED", "count": 1}]
