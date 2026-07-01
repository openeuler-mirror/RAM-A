"""Helpers for prepared benchmark files and search output."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any


def load_prepared(path: Path) -> dict[str, Any]:
    with open(path, "r", encoding="utf-8") as f:
        prepared = json.load(f)
    if prepared.get("schema_version") != "benchmark-prepared-v1":
        raise ValueError(f"{path} is not benchmark-prepared-v1")
    return prepared


def write_search_results(path: Path, results: list[dict[str, Any]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        json.dump(results, f, ensure_ascii=False, indent=2)


def make_query_output(query: dict[str, Any], results: list[dict[str, Any]]) -> dict[str, Any]:
    return {
        "query_path": None,
        "query": query["text"],
        "query_id": query.get("id"),
        "filter": query.get("filter"),
        "metadata": query.get("metadata"),
        "task": query.get("task"),
        "results": results,
    }
