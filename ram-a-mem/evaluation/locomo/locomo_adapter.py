"""Convert LoCoMo datasets into the benchmark-prepared-v1 contract."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path
from typing import Any, Sequence


PREPARED_SCHEMA_VERSION = "benchmark-prepared-v1"
SESSION_RE = re.compile(r"^session_(\d+)$")


def prepare_locomo(
    dataset: list[dict[str, Any]],
    sample_indexes: tuple[int, ...] | None = None,
) -> dict[str, Any]:
    """Map selected LoCoMo samples to stable raw memories and queries."""

    selected = _selected_indexes(dataset, sample_indexes)
    memories: list[dict[str, Any]] = []
    queries: list[dict[str, Any]] = []

    for sample_index, sample in enumerate(dataset):
        if sample_index not in selected:
            continue
        conversation = sample["conversation"]
        scope_id = f"locomo:S{sample_index}"
        speaker_a = str(conversation["speaker_a"])
        speaker_b = str(conversation["speaker_b"])

        for session_number in _session_numbers(conversation):
            session_key = f"session_{session_number}"
            timestamp = str(conversation[f"{session_key}_date_time"])
            for turn_index, turn in enumerate(conversation[session_key]):
                dia_id = str(turn["dia_id"])
                speaker = str(turn["speaker"])
                if speaker == speaker_a:
                    role = "speaker_a"
                elif speaker == speaker_b:
                    role = "speaker_b"
                else:
                    raise ValueError(
                        f"sample {sample_index} has unknown speaker {speaker!r}"
                    )
                memories.append(
                    {
                        "id": f"S{sample_index}:{dia_id}",
                        "text": str(turn["text"]),
                        "metadata": {
                            "memory_kind": "raw_turn",
                            "scope_id": scope_id,
                            "session_id": f"S{sample_index}:{session_key}",
                            "sample_index": sample_index,
                            "session_number": session_number,
                            "turn_index": turn_index,
                            "dia_id": dia_id,
                            "speaker": speaker,
                            "role": role,
                            "timestamp": timestamp,
                        },
                    }
                )

        for question_index, question in enumerate(sample["qa"]):
            queries.append(
                {
                    "id": f"S{sample_index}:Q{question_index}",
                    "text": str(question["question"]),
                    "filter": {"scope_id": scope_id},
                    "metadata": {
                        "sample_index": sample_index,
                        "question_index": question_index,
                    },
                    "task": {
                        "sample_index": sample_index,
                        "question_index": question_index,
                        "category": int(question["category"]),
                        "answer": str(question.get("answer") or ""),
                        "evidence_ids": [
                            f"S{sample_index}:{evidence_id}"
                            for evidence_id in question.get("evidence", [])
                        ],
                    },
                }
            )

    prepared = {
        "schema_version": PREPARED_SCHEMA_VERSION,
        "dataset": {
            "name": "locomo",
            "sample_indexes": sorted(selected),
        },
        "memories": memories,
        "queries": queries,
    }
    source_lookup(prepared)
    return prepared


def source_lookup(
    prepared: dict[str, Any],
) -> dict[str, dict[str, Any]]:
    """Index prepared source records by stable memory ID."""

    if prepared.get("schema_version") != PREPARED_SCHEMA_VERSION:
        raise ValueError(f"prepared input must use {PREPARED_SCHEMA_VERSION}")
    result: dict[str, dict[str, Any]] = {}
    for record in prepared.get("memories", []):
        memory_id = str(record.get("id") or "")
        if memory_id in result:
            raise ValueError(f"duplicate source memory id: {memory_id}")
        result[memory_id] = record
    return result


def _selected_indexes(
    dataset: list[dict[str, Any]],
    sample_indexes: tuple[int, ...] | None,
) -> set[int]:
    if sample_indexes is None:
        return set(range(len(dataset)))
    selected = set(sample_indexes)
    for sample_index in sorted(selected):
        if sample_index < 0 or sample_index >= len(dataset):
            raise ValueError(f"sample index out of range: {sample_index}")
    return selected


def _session_numbers(conversation: dict[str, Any]) -> list[int]:
    numbers = []
    for key, value in conversation.items():
        match = SESSION_RE.fullmatch(key)
        if match and isinstance(value, list):
            numbers.append(int(match.group(1)))
    return sorted(numbers)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Convert LoCoMo JSON into benchmark-prepared-v1."
    )
    parser.add_argument("--dataset", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--sample-index", type=int, action="append")
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    dataset = json.loads(args.dataset.read_text(encoding="utf-8"))
    if not isinstance(dataset, list):
        raise ValueError("LoCoMo dataset must be a JSON array")
    selected = tuple(args.sample_index) if args.sample_index is not None else None
    prepared = prepare_locomo(dataset, selected)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(prepared, ensure_ascii=False, sort_keys=True, indent=2) + "\n",
        encoding="utf-8",
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
