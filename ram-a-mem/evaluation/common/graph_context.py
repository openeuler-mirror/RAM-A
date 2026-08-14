"""Dataset-independent rendering for graph facts attached to search results."""

from __future__ import annotations

from datetime import datetime, timezone
import re
from typing import Any


DEFAULT_MAX_GRAPH_CONTEXT_FACTS = 3


class GraphFactContextRenderer:
    """Render graph facts under one shared per-question budget."""

    def __init__(self, *, max_facts: int = DEFAULT_MAX_GRAPH_CONTEXT_FACTS) -> None:
        self.max_facts = max_facts
        self._rendered_count = 0
        self._seen_ids: set[str] = set()
        self._seen_texts: set[str] = set()

    def enrich(self, text: Any, metadata: Any) -> str:
        base_text = str(text or "")
        if self._rendered_count >= self.max_facts or not isinstance(metadata, dict):
            return base_text

        raw_facts = metadata.get("graph_facts")
        if not isinstance(raw_facts, list):
            return base_text

        normalized_base = _normalize_text(base_text)
        rendered_facts: list[str] = []
        for raw_fact in raw_facts:
            if not isinstance(raw_fact, dict):
                continue
            fact_text = _single_line(raw_fact.get("fact_text"))
            if not fact_text:
                continue

            fact_id = raw_fact.get("fact_id")
            normalized_fact = _normalize_text(fact_text)
            normalized_fact_id = fact_id if isinstance(fact_id, str) else ""
            if (
                normalized_fact_id
                and normalized_fact_id in self._seen_ids
                or normalized_fact in self._seen_texts
            ):
                continue
            if normalized_fact_id:
                self._seen_ids.add(normalized_fact_id)
            self._seen_texts.add(normalized_fact)

            if _contains_normalized_phrase(normalized_base, normalized_fact):
                continue

            predicate = _single_line(raw_fact.get("predicate"))
            rendered = f"[{predicate}] {fact_text}" if predicate else fact_text
            validity = format_graph_fact_validity(raw_fact)
            if validity:
                rendered += f" [{validity}]"
            rendered_facts.append(rendered)
            self._rendered_count += 1
            if self._rendered_count >= self.max_facts:
                break

        if not rendered_facts:
            return base_text
        facts = "\n".join(f"- {fact}" for fact in rendered_facts)
        return f"{base_text}\nMatched graph facts:\n{facts}"


def enrich_text_with_graph_facts(
    text: Any,
    metadata: Any,
    *,
    max_facts: int = DEFAULT_MAX_GRAPH_CONTEXT_FACTS,
) -> str:
    """Append graph facts using a new one-memory rendering budget."""
    return GraphFactContextRenderer(max_facts=max_facts).enrich(text, metadata)


def _single_line(value: Any) -> str:
    if not isinstance(value, str):
        return ""
    return " ".join(value.split())


def _normalize_text(value: str) -> str:
    return " ".join(re.sub(r"[^\w]+", " ", value.casefold()).split())


def _contains_normalized_phrase(text: str, phrase: str) -> bool:
    if not phrase:
        return False
    if any(ord(character) > 127 for character in phrase):
        return phrase in text
    return f" {phrase} " in f" {text} "


def format_graph_fact_validity(fact: dict[str, Any]) -> str | None:
    """Render trusted validity bounds without using ingestion time."""
    valid_from = _format_timestamp_ms(fact.get("valid_from_ms"))
    valid_to = _format_timestamp_ms(fact.get("valid_to_ms"))
    if valid_from and valid_to:
        if valid_from == valid_to:
            return f"valid at {valid_from}"
        return f"valid from {valid_from} to {valid_to}"
    if valid_from:
        return f"valid from {valid_from}"
    if valid_to:
        return f"valid until {valid_to}"
    return None


def _format_timestamp_ms(value: Any) -> str | None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    try:
        return (
            datetime.fromtimestamp(value / 1000, tz=timezone.utc)
            .isoformat()
            .replace("+00:00", "Z")
        )
    except (OSError, OverflowError, ValueError):
        return None
