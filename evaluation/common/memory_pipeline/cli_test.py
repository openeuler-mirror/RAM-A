from __future__ import annotations

import json

import pytest

from common.memory_pipeline.cli import main
from common.memory_pipeline.episode import EpisodeConfig, build_episodes
from common.memory_pipeline.normalize import normalize_prepared_memories
from common.memory_pipeline.validation import ValidationConfig, validate_extraction
from common.memory_pipeline.window import WindowConfig, build_windows


PREPARED = {
    "schema_version": "benchmark-prepared-v1",
    "dataset": {"name": "cli-fixture", "split": "test"},
    "memories": [
        {
            "id": "m1",
            "text": "I plan to move to Hangzhou in August.",
            "metadata": {
                "scope_id": "u1",
                "session_id": "s1",
                "role": "user",
                "speaker": "Alice",
                "timestamp": "2026-07-14T10:00:00Z",
            },
        }
    ],
    "queries": [],
}


def _raw_memory() -> dict:
    return {
        "text": "Alice plans to move to Hangzhou in August 2026.",
        "memory_type": "event",
        "subject": {"name": "Alice", "source_speaker": "Alice"},
        "predicate": "plans_to_move_to",
        "object": {"name": "Hangzhou", "type": "place"},
        "modality": "planned",
        "event_time": {
            "raw": "in August",
            "normalized": "2026-08",
            "precision": "month",
        },
        "attributes": {},
        "evidence": [
            {
                "message_id": "m1",
                "quote": "plan to move to Hangzhou in August",
                "evidence_role": "primary",
            }
        ],
        "model_confidence": 0.9,
    }


def _write_fixture_files(tmp_path):
    input_path = tmp_path / "input.json"
    extraction_path = tmp_path / "extraction.json"
    grounding_path = tmp_path / "grounding.json"
    input_path.write_text(json.dumps(PREPARED), encoding="utf-8")

    messages, _ = normalize_prepared_memories(PREPARED)
    lookup = {message.id: message for message in messages}
    episodes = build_episodes(messages, EpisodeConfig())
    window_config = WindowConfig(max_candidate_tokens=64, max_window_tokens=128)
    window = build_windows(episodes, lookup, window_config)[0]
    raw_memory = _raw_memory()
    extraction_path.write_text(
        json.dumps(
            {
                window.id: {
                    "schema_version": "atomic_memory_v1",
                    "memories": [raw_memory],
                }
            }
        ),
        encoding="utf-8",
    )
    candidate = validate_extraction(
        [raw_memory], window, lookup, ValidationConfig()
    ).valid[0]
    grounding_path.write_text(
        json.dumps({candidate.id: "SUPPORTED"}), encoding="utf-8"
    )
    return input_path, extraction_path, grounding_path


def test_cli_fixture_mode_writes_prepared_and_artifacts(tmp_path) -> None:
    input_path, extraction_path, grounding_path = _write_fixture_files(tmp_path)
    output_path = tmp_path / "out" / "prepared.json"
    artifacts = tmp_path / "artifacts"

    result = main(
        [
            "--input",
            str(input_path),
            "--output",
            str(output_path),
            "--artifacts-dir",
            str(artifacts),
            "--extractor-responses",
            str(extraction_path),
            "--grounding-responses",
            str(grounding_path),
            "--max-candidate-tokens",
            "64",
            "--max-window-tokens",
            "128",
        ]
    )

    assert result == 0
    prepared = json.loads(output_path.read_text(encoding="utf-8"))
    assert prepared["memories"][0]["metadata"]["memory_kind"] == "extracted_memory"
    assert json.loads((artifacts / "prepared.json").read_text()) == prepared


def test_cli_refuses_fixture_mode_without_grounding(tmp_path) -> None:
    input_path, extraction_path, _ = _write_fixture_files(tmp_path)
    output_path = tmp_path / "prepared.json"

    with pytest.raises(SystemExit):
        main(
            [
                "--input",
                str(input_path),
                "--output",
                str(output_path),
                "--artifacts-dir",
                str(tmp_path / "artifacts"),
                "--extractor-responses",
                str(extraction_path),
            ]
        )

    assert not output_path.exists()
