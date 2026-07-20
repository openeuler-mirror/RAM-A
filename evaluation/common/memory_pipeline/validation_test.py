from __future__ import annotations

import pytest

from common.memory_pipeline.models import ExtractionWindow, MessageRef, NormalizedMessage
from common.memory_pipeline.validation import ValidationConfig, validate_extraction


LOOKUP = {
    "context": NormalizedMessage(
        id="context",
        scope_id="u1",
        session_id="s1",
        text="Alice used to live in Shanghai.",
        role="user",
        speaker="Alice",
    ),
    "candidate": NormalizedMessage(
        id="candidate",
        scope_id="u1",
        session_id="s1",
        text="I plan to move to Hangzhou in August.",
        role="user",
        speaker="Alice",
        timestamp="2026-07-14T10:00:00Z",
    ),
    "duplicate": NormalizedMessage(
        id="duplicate",
        scope_id="u1",
        session_id="s1",
        text="coffee coffee",
        role="user",
    ),
}
WINDOW = ExtractionWindow(
    id="window-1",
    scope_id="u1",
    session_id="s1",
    episode_id="episode-1",
    context_before_refs=(
        MessageRef("context", 0, len(LOOKUP["context"].text), LOOKUP["context"].text),
    ),
    candidate_refs=(
        MessageRef("candidate", 0, len(LOOKUP["candidate"].text), LOOKUP["candidate"].text),
    ),
)


def _raw_memory(
    *,
    modality: str = "planned",
    evidence: list[dict] | None = None,
    confidence: float | None = 0.99,
) -> dict:
    value = {
        "text": "Alice plans to move to Hangzhou in August 2026.",
        "memory_type": "event",
        "subject": {"name": "Alice", "source_speaker": "Alice"},
        "predicate": "plans_to_move_to",
        "object": {"name": "Hangzhou", "type": "place"},
        "modality": modality,
        "event_time": {
            "raw": "in August",
            "normalized": "2026-08",
            "precision": "month",
        },
        "attributes": {},
        "evidence": evidence
        if evidence is not None
        else [
            {
                "message_id": "candidate",
                "quote": "plan to move to Hangzhou in August",
                "evidence_role": "primary",
            }
        ],
    }
    if confidence is not None:
        value["model_confidence"] = confidence
    return value


def test_candidate_exact_quote_becomes_host_computed_offsets() -> None:
    result = validate_extraction([_raw_memory()], WINDOW, LOOKUP, ValidationConfig())

    assert result.rejected == []
    assert result.quarantined == []
    memory = result.valid[0]
    evidence = memory.evidence[0]
    expected_start = LOOKUP["candidate"].text.index("plan to move to Hangzhou in August")
    assert evidence.start_char == expected_start
    assert evidence.end_char == expected_start + len(evidence.quote)
    assert memory.observed_at == "2026-07-14T10:00:00Z"


@pytest.mark.parametrize("invalid_object", [["Hangzhou"], 42, True])
def test_schema_rejects_non_object_value_types(invalid_object) -> None:
    raw = _raw_memory()
    raw["object"] = invalid_object

    result = validate_extraction([raw], WINDOW, LOOKUP, ValidationConfig())

    assert result.valid == []
    assert result.rejected[0].code == "malformed_schema"


def test_context_only_evidence_is_rejected() -> None:
    raw = _raw_memory(
        evidence=[
            {
                "message_id": "context",
                "quote": "used to live in Shanghai",
                "evidence_role": "supporting",
            }
        ]
    )

    result = validate_extraction([raw], WINDOW, LOOKUP, ValidationConfig())

    assert result.valid == []
    assert result.rejected[0].code == "missing_candidate_evidence"


def test_duplicate_quote_is_quarantined() -> None:
    duplicate_window = ExtractionWindow(
        id="window-duplicate",
        scope_id="u1",
        session_id="s1",
        episode_id="episode-1",
        candidate_refs=(MessageRef("duplicate", 0, 13, "coffee coffee"),),
    )
    raw = _raw_memory(
        evidence=[
            {
                "message_id": "duplicate",
                "quote": "coffee",
                "evidence_role": "primary",
            }
        ]
    )

    result = validate_extraction([raw], duplicate_window, LOOKUP, ValidationConfig())

    assert result.valid == []
    assert result.quarantined[0].code == "ambiguous_evidence_quote"


def test_quote_outside_candidate_slice_is_quarantined() -> None:
    sliced_window = ExtractionWindow(
        id="window-slice",
        scope_id="u1",
        session_id="s1",
        episode_id="episode-1",
        candidate_refs=(MessageRef("candidate", 0, 6, "I plan"),),
    )

    result = validate_extraction([_raw_memory()], sliced_window, LOOKUP, ValidationConfig())

    assert result.valid == []
    assert result.quarantined[0].code == "evidence_quote_not_found"


def test_unknown_enums_and_invalid_confidence_are_schema_rejections() -> None:
    bad_type = _raw_memory()
    bad_type["memory_type"] = "unknown-type"
    bad_confidence = _raw_memory(confidence=1.5)

    result = validate_extraction(
        [bad_type, bad_confidence],
        WINDOW,
        LOOKUP,
        ValidationConfig(),
    )

    assert [issue.code for issue in result.rejected] == ["unknown_enum", "malformed_schema"]


def test_high_confidence_does_not_override_suspicious_modality() -> None:
    result = validate_extraction(
        [_raw_memory(modality="asserted", confidence=1.0)],
        WINDOW,
        LOOKUP,
        ValidationConfig(),
    )

    assert result.valid == []
    assert result.quarantined[0].code == "suspicious_modality"


def test_empty_extraction_is_valid_no_write() -> None:
    result = validate_extraction([], WINDOW, LOOKUP, ValidationConfig())

    assert result.valid == []
    assert result.rejected == []
    assert result.quarantined == []
