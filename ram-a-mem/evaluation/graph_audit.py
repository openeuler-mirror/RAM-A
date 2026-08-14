#!/usr/bin/env python3
"""Report graph-memory coverage and lifecycle statistics from a SQLite store."""

import argparse
import json
import sqlite3
import sys
from pathlib import Path


def scoped_clause(memory_space_id):
    if memory_space_id is None:
        return "", []
    return " WHERE memory_space_id = ?", [memory_space_id]


def scoped_and(memory_space_id, condition):
    if memory_space_id is None:
        return f" WHERE {condition}", []
    return f" WHERE memory_space_id = ? AND {condition}", [memory_space_id]


def scalar(connection, query, params=()):
    return connection.execute(query, params).fetchone()[0]


def rows(connection, query, params=()):
    columns = [column[0] for column in connection.execute(query, params).description]
    return [dict(zip(columns, row)) for row in connection.execute(query, params)]


def has_table(connection, table_name):
    return (
        connection.execute(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?", [table_name]
        ).fetchone()
        is not None
    )


def build_audit(connection, memory_space_id=None):
    scope, scope_params = scoped_clause(memory_space_id)
    active_scope, active_scope_params = scoped_and(memory_space_id, "deleted_at_ms IS NULL")
    active_fact_scope, active_fact_scope_params = scoped_and(
        memory_space_id, "status = 'active' AND retired_at_ms IS NULL"
    )
    active_entity_scope, active_entity_scope_params = scoped_and(
        memory_space_id, "status = 'active' AND deleted_at_ms IS NULL"
    )

    records_with_evidence = scalar(
        connection,
        """
        SELECT count(DISTINCT r.id)
        FROM graph_memory_records r
        WHERE r.deleted_at_ms IS NULL
          AND (? IS NULL OR r.memory_space_id = ?)
          AND EXISTS (
              SELECT 1
              FROM graph_fact_evidence e
              WHERE e.memory_space_id = r.memory_space_id
                AND e.memory_record_id = r.id
                AND e.deleted_at_ms IS NULL
          )
        """,
        [memory_space_id, memory_space_id],
    )
    records = scalar(
        connection,
        f"SELECT count(*) FROM graph_memory_records{active_scope}",
        active_scope_params,
    )
    active_facts = scalar(
        connection,
        f"SELECT count(*) FROM graph_facts{active_fact_scope}",
        active_fact_scope_params,
    )
    facts_with_evidence = scalar(
        connection,
        """
        SELECT count(DISTINCT f.id)
        FROM graph_facts f
        JOIN graph_fact_evidence_groups g
          ON g.memory_space_id = f.memory_space_id
         AND g.fact_id = f.id
         AND g.deleted_at_ms IS NULL
        JOIN graph_fact_evidence e
          ON e.memory_space_id = g.memory_space_id
         AND e.evidence_group_id = g.id
         AND e.deleted_at_ms IS NULL
        WHERE f.status = 'active'
          AND f.retired_at_ms IS NULL
          AND (? IS NULL OR f.memory_space_id = ?)
        """,
        [memory_space_id, memory_space_id],
    )
    record_entity_links_supported = has_table(connection, "graph_record_entity_links")
    if record_entity_links_supported:
        records_with_entity_link = scalar(
            connection,
            """
            SELECT count(DISTINCT r.id)
            FROM graph_memory_records r
            JOIN graph_record_entity_links links
              ON links.memory_record_id = r.id
             AND links.memory_space_id = r.memory_space_id
            WHERE r.deleted_at_ms IS NULL
              AND (? IS NULL OR r.memory_space_id = ?)
            """,
            [memory_space_id, memory_space_id],
        )
        record_entity_links = scalar(
            connection,
            """
            SELECT count(*)
            FROM graph_record_entity_links
            WHERE ? IS NULL OR memory_space_id = ?
            """,
            [memory_space_id, memory_space_id],
        )
    else:
        records_with_entity_link = 0
        record_entity_links = 0

    summary = {
        "memory_spaces": scalar(
            connection,
            f"SELECT count(*) FROM graph_memory_spaces{scope.replace('memory_space_id', 'id')}",
            scope_params,
        ),
        "records": records,
        "active_entities": scalar(
            connection,
            f"SELECT count(*) FROM graph_entities{active_entity_scope}",
            active_entity_scope_params,
        ),
        "aliases": scalar(
            connection,
            f"SELECT count(*) FROM graph_entity_aliases{active_scope}",
            active_scope_params,
        ),
        "active_facts": active_facts,
        "evidence_links": scalar(
            connection,
            f"SELECT count(*) FROM graph_fact_evidence{active_scope}",
            active_scope_params,
        ),
        "facts_with_evidence": facts_with_evidence,
        "facts_without_evidence": active_facts - facts_with_evidence,
        "records_with_fact_evidence": records_with_evidence,
        "records_without_fact_evidence": records - records_with_evidence,
        "record_fact_evidence_coverage": round(records_with_evidence / records, 6)
        if records
        else 0.0,
        "record_entity_links_supported": record_entity_links_supported,
        "record_entity_links": record_entity_links,
        "records_with_entity_link": records_with_entity_link,
        "records_without_entity_link": records - records_with_entity_link,
        "record_entity_link_coverage": round(records_with_entity_link / records, 6)
        if records
        else 0.0,
    }

    return {
        "memory_space_id": memory_space_id,
        "summary": summary,
        "predicate_distribution": rows(
            connection,
            f"""
            SELECT predicate, count(*) AS count
            FROM graph_facts{active_fact_scope}
            GROUP BY predicate
            ORDER BY count DESC, predicate ASC
            """,
            active_fact_scope_params,
        ),
        "entity_type_distribution": rows(
            connection,
            f"""
            SELECT entity_type, count(*) AS count
            FROM graph_entities{active_entity_scope}
            GROUP BY entity_type
            ORDER BY count DESC, entity_type ASC
            """,
            active_entity_scope_params,
        ),
        "ingestion_status_stage_distribution": rows(
            connection,
            f"""
            SELECT status, stage, count(*) AS count
            FROM graph_ingestion_runs{scope}
            GROUP BY status, stage
            ORDER BY status ASC, stage ASC
            """,
            scope_params,
        ),
        "extraction_status_distribution": rows(
            connection,
            f"""
            SELECT status, count(*) AS count
            FROM graph_extraction_runs{scope}
            GROUP BY status
            ORDER BY status ASC
            """,
            scope_params,
        ),
        "record_entity_link_kind_distribution": (
            rows(
                connection,
                f"""
                SELECT link_kind, count(*) AS count
                FROM graph_record_entity_links{scope}
                GROUP BY link_kind
                ORDER BY count DESC, link_kind ASC
                """,
                scope_params,
            )
            if record_entity_links_supported
            else []
        ),
    }


def parse_args():
    parser = argparse.ArgumentParser(
        description="Audit graph-memory coverage and lifecycle status from a SQLite store."
    )
    parser.add_argument("--store", required=True, type=Path, help="SQLite memory store")
    parser.add_argument("--memory-space-id", help="Optional graph memory-space filter")
    parser.add_argument("--output", type=Path, help="Optional JSON output path")
    return parser.parse_args()


def main():
    args = parse_args()
    if not args.store.is_file():
        raise SystemExit(f"store does not exist: {args.store}")

    connection = sqlite3.connect(f"file:{args.store}?mode=ro", uri=True)
    try:
        audit = build_audit(connection, args.memory_space_id)
    finally:
        connection.close()

    content = json.dumps(audit, ensure_ascii=False, indent=2) + "\n"
    if args.output is None:
        sys.stdout.write(content)
    else:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(content, encoding="utf-8")
        print(f"graph audit saved to {args.output}")


if __name__ == "__main__":
    main()
