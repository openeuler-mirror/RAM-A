use rusqlite::Connection;

use crate::MemoryResult;

pub fn initialize_schema(connection: &Connection) -> MemoryResult<()> {
    connection.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;

        CREATE TABLE IF NOT EXISTS graph_memory_spaces (
            id TEXT NOT NULL,
            owner_id TEXT NOT NULL,
            status TEXT NOT NULL,
            next_ingestion_sequence INTEGER NOT NULL DEFAULT 1,
            created_at_ms INTEGER NOT NULL,
            deleted_at_ms INTEGER,
            PRIMARY KEY (id)
        );

        CREATE TABLE IF NOT EXISTS graph_memory_records (
            id TEXT NOT NULL,
            memory_space_id TEXT NOT NULL,
            session_id TEXT,
            ingestion_sequence INTEGER NOT NULL,
            session_sequence INTEGER,
            text TEXT NOT NULL,
            metadata_json TEXT NOT NULL,
            source_kind TEXT NOT NULL,
            source_ref TEXT,
            content_role TEXT NOT NULL,
            created_by_agent_id TEXT,
            observed_at_ms INTEGER,
            embedding BLOB,
            embedding_dims INTEGER,
            embedding_model TEXT,
            embedding_version TEXT,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            deleted_at_ms INTEGER,
            PRIMARY KEY (id),
            UNIQUE (id, memory_space_id),
            FOREIGN KEY (memory_space_id) REFERENCES graph_memory_spaces(id)
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS graph_memory_record_fts
        USING fts5(id UNINDEXED, memory_space_id UNINDEXED, text);

        CREATE TABLE IF NOT EXISTS graph_entities (
            id TEXT NOT NULL,
            memory_space_id TEXT NOT NULL,
            canonical_name TEXT NOT NULL,
            normalized_name TEXT NOT NULL,
            entity_type TEXT NOT NULL,
            name_embedding BLOB,
            embedding_dims INTEGER,
            embedding_model TEXT,
            embedding_version TEXT,
            status TEXT NOT NULL,
            type_registry_version TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            deleted_at_ms INTEGER,
            PRIMARY KEY (id),
            UNIQUE (id, memory_space_id),
            FOREIGN KEY (memory_space_id) REFERENCES graph_memory_spaces(id)
        );

        CREATE INDEX IF NOT EXISTS idx_graph_entities_space_name
        ON graph_entities(memory_space_id, normalized_name, status);

        CREATE VIRTUAL TABLE IF NOT EXISTS graph_entity_fts
        USING fts5(id UNINDEXED, memory_space_id UNINDEXED, canonical_name);

        CREATE TABLE IF NOT EXISTS graph_entity_aliases (
            id TEXT NOT NULL,
            memory_space_id TEXT NOT NULL,
            entity_id TEXT NOT NULL,
            display_alias TEXT NOT NULL,
            normalized_alias TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL,
            deleted_at_ms INTEGER,
            PRIMARY KEY (id),
            UNIQUE (id, memory_space_id),
            FOREIGN KEY (entity_id, memory_space_id)
                REFERENCES graph_entities(id, memory_space_id)
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS graph_entity_alias_fts
        USING fts5(id UNINDEXED, memory_space_id UNINDEXED, display_alias);

        CREATE TABLE IF NOT EXISTS graph_facts (
            id TEXT NOT NULL,
            memory_space_id TEXT NOT NULL,
            subject_entity_id TEXT NOT NULL,
            predicate TEXT NOT NULL,
            object_entity_id TEXT NOT NULL,
            fact_text TEXT NOT NULL,
            dedup_key TEXT,
            embedding BLOB,
            embedding_dims INTEGER,
            embedding_model TEXT,
            embedding_version TEXT,
            status TEXT NOT NULL,
            valid_from_ms INTEGER,
            valid_to_ms INTEGER,
            recorded_at_ms INTEGER NOT NULL,
            retired_at_ms INTEGER,
            type_registry_version TEXT NOT NULL,
            PRIMARY KEY (id),
            UNIQUE (id, memory_space_id),
            FOREIGN KEY (subject_entity_id, memory_space_id)
                REFERENCES graph_entities(id, memory_space_id),
            FOREIGN KEY (object_entity_id, memory_space_id)
                REFERENCES graph_entities(id, memory_space_id)
        );

        CREATE INDEX IF NOT EXISTS idx_graph_facts_subject_status
        ON graph_facts(memory_space_id, subject_entity_id, status);

        CREATE INDEX IF NOT EXISTS idx_graph_facts_object_status
        ON graph_facts(memory_space_id, object_entity_id, status);

        CREATE VIRTUAL TABLE IF NOT EXISTS graph_fact_fts
        USING fts5(id UNINDEXED, memory_space_id UNINDEXED, fact_text);
        "#,
    )?;

    initialize_evidence_and_run_schema(connection)?;
    Ok(())
}

fn initialize_evidence_and_run_schema(connection: &Connection) -> MemoryResult<()> {
    connection.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS graph_fact_evidence_groups (
            id TEXT NOT NULL,
            memory_space_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            evidence_kind TEXT NOT NULL,
            extraction_run_id TEXT,
            created_at_ms INTEGER NOT NULL,
            deleted_at_ms INTEGER,
            PRIMARY KEY (id),
            UNIQUE (id, memory_space_id),
            FOREIGN KEY (fact_id, memory_space_id)
                REFERENCES graph_facts(id, memory_space_id)
        );

        CREATE TABLE IF NOT EXISTS graph_fact_evidence (
            id TEXT NOT NULL,
            memory_space_id TEXT NOT NULL,
            evidence_group_id TEXT NOT NULL,
            memory_record_id TEXT NOT NULL,
            evidence_text TEXT,
            start_byte INTEGER,
            end_byte INTEGER,
            created_at_ms INTEGER NOT NULL,
            deleted_at_ms INTEGER,
            PRIMARY KEY (id),
            FOREIGN KEY (evidence_group_id, memory_space_id)
                REFERENCES graph_fact_evidence_groups(id, memory_space_id),
            FOREIGN KEY (memory_record_id, memory_space_id)
                REFERENCES graph_memory_records(id, memory_space_id)
        );

        CREATE TABLE IF NOT EXISTS graph_fact_links (
            id TEXT NOT NULL,
            memory_space_id TEXT NOT NULL,
            from_fact_id TEXT NOT NULL,
            link_type TEXT NOT NULL,
            to_fact_id TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL,
            deleted_at_ms INTEGER,
            PRIMARY KEY (id),
            UNIQUE (id, memory_space_id),
            FOREIGN KEY (from_fact_id, memory_space_id)
                REFERENCES graph_facts(id, memory_space_id),
            FOREIGN KEY (to_fact_id, memory_space_id)
                REFERENCES graph_facts(id, memory_space_id)
        );

        CREATE TABLE IF NOT EXISTS graph_fact_link_evidence_groups (
            id TEXT NOT NULL,
            memory_space_id TEXT NOT NULL,
            fact_link_id TEXT NOT NULL,
            extraction_run_id TEXT,
            created_at_ms INTEGER NOT NULL,
            deleted_at_ms INTEGER,
            PRIMARY KEY (id),
            UNIQUE (id, memory_space_id),
            FOREIGN KEY (fact_link_id, memory_space_id)
                REFERENCES graph_fact_links(id, memory_space_id)
        );

        CREATE TABLE IF NOT EXISTS graph_fact_link_evidence (
            id TEXT NOT NULL,
            memory_space_id TEXT NOT NULL,
            evidence_group_id TEXT NOT NULL,
            memory_record_id TEXT NOT NULL,
            evidence_text TEXT,
            start_byte INTEGER,
            end_byte INTEGER,
            created_at_ms INTEGER NOT NULL,
            deleted_at_ms INTEGER,
            PRIMARY KEY (id),
            FOREIGN KEY (evidence_group_id, memory_space_id)
                REFERENCES graph_fact_link_evidence_groups(id, memory_space_id),
            FOREIGN KEY (memory_record_id, memory_space_id)
                REFERENCES graph_memory_records(id, memory_space_id)
        );

        CREATE TABLE IF NOT EXISTS graph_ingestion_runs (
            id TEXT NOT NULL,
            memory_space_id TEXT NOT NULL,
            memory_record_id TEXT NOT NULL,
            idempotency_key TEXT NOT NULL,
            input_hash TEXT NOT NULL,
            status TEXT NOT NULL,
            stage TEXT NOT NULL,
            attempt_count INTEGER NOT NULL,
            pipeline_version TEXT NOT NULL,
            error_code TEXT,
            error_message TEXT,
            created_at_ms INTEGER NOT NULL,
            started_at_ms INTEGER,
            updated_at_ms INTEGER NOT NULL,
            completed_at_ms INTEGER,
            PRIMARY KEY (id),
            UNIQUE (id, memory_space_id),
            UNIQUE (memory_space_id, idempotency_key),
            FOREIGN KEY (memory_record_id, memory_space_id)
                REFERENCES graph_memory_records(id, memory_space_id)
        );

        CREATE TABLE IF NOT EXISTS graph_extraction_runs (
            id TEXT NOT NULL PRIMARY KEY,
            memory_space_id TEXT NOT NULL,
            ingestion_run_id TEXT NOT NULL,
            attempt_number INTEGER NOT NULL,
            status TEXT NOT NULL,
            extractor_name TEXT NOT NULL,
            model TEXT NOT NULL,
            prompt_version TEXT NOT NULL,
            schema_version TEXT NOT NULL,
            type_registry_version TEXT NOT NULL,
            context_record_ids_json TEXT NOT NULL,
            structured_output_json TEXT,
            input_tokens INTEGER,
            output_tokens INTEGER,
            latency_ms INTEGER,
            error_code TEXT,
            error_message TEXT,
            created_at_ms INTEGER NOT NULL,
            completed_at_ms INTEGER,
            FOREIGN KEY (ingestion_run_id, memory_space_id)
                REFERENCES graph_ingestion_runs(id, memory_space_id)
        );

        CREATE TABLE IF NOT EXISTS graph_resolution_decisions (
            id TEXT NOT NULL PRIMARY KEY,
            memory_space_id TEXT NOT NULL,
            ingestion_run_id TEXT NOT NULL,
            decision_kind TEXT NOT NULL,
            input_key TEXT NOT NULL,
            candidate_ids_json TEXT NOT NULL,
            selected_id TEXT,
            action TEXT NOT NULL,
            method TEXT NOT NULL,
            model TEXT,
            resolver_version TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL,
            FOREIGN KEY (ingestion_run_id, memory_space_id)
                REFERENCES graph_ingestion_runs(id, memory_space_id)
        );

        CREATE TABLE IF NOT EXISTS graph_fact_status_history (
            id TEXT NOT NULL PRIMARY KEY,
            memory_space_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            old_status TEXT,
            new_status TEXT NOT NULL,
            reason_code TEXT NOT NULL,
            trigger_record_id TEXT,
            trigger_fact_link_id TEXT,
            created_at_ms INTEGER NOT NULL,
            FOREIGN KEY (fact_id, memory_space_id)
                REFERENCES graph_facts(id, memory_space_id),
            FOREIGN KEY (trigger_record_id, memory_space_id)
                REFERENCES graph_memory_records(id, memory_space_id),
            FOREIGN KEY (trigger_fact_link_id, memory_space_id)
                REFERENCES graph_fact_links(id, memory_space_id)
        );
        "#,
    )?;
    Ok(())
}
