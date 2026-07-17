from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace

from common.memory_pipeline.cache import JsonCache
from locomo.locomo_adapter import prepare_locomo
import locomo.locomo_responses as responses_module
from locomo.locomo_responses import PreparedMemoryResponses, ResponseClient


FIXTURE = Path(__file__).parents[1] / "fixtures" / "locomo_sample.json"


class FakeResponder:
    model = "openai/gpt-4o-mini"

    def __init__(self) -> None:
        self.calls: list[tuple] = []

    def answer(self, *args):
        self.calls.append(args)
        return (
            "On the kitchen table.",
            0.01,
            {"prompt_tokens": 8, "completion_tokens": 4, "total_tokens": 12},
        )


def _dataset_and_prepared():
    dataset = json.loads(FIXTURE.read_text(encoding="utf-8"))
    return dataset, prepare_locomo(dataset)


def _extracted_query():
    return {
        "query_id": "S0:Q0",
        "task": {"sample_index": 0, "question_index": 0},
        "results": [
            {
                "id": "mem-1",
                "text": "Alex left a blue notebook on the kitchen table.",
                "score": 0.9,
                "metadata": {
                    "memory_kind": "extracted_memory",
                    "evidence_refs": [
                        {
                            "message_id": "S0:D1:0",
                            "quote": "blue notebook",
                            "start_char": 10,
                            "end_char": 23,
                            "evidence_role": "support",
                        }
                    ],
                },
            }
        ],
    }


def test_prepared_extracted_answer_uses_atomic_claim_and_original_evidence() -> None:
    dataset, prepared = _dataset_and_prepared()
    responder = FakeResponder()
    generator = PreparedMemoryResponses("extracted", responder=responder)

    sample_index, answer = generator.answer_question(
        dataset,
        prepared,
        _extracted_query(),
    )

    joined = "\n".join(responder.calls[0][2] + responder.calls[0][3])
    assert sample_index == 0
    assert "[Atomic]" in joined
    assert "[Evidence S0:D1:0]" in joined
    assert "before leaving for work" in joined
    assert answer["query_id"] == "S0:Q0"
    assert answer["response"] == "On the kitchen table."
    assert answer["total_tokens"] == 12


def test_prepared_category_five_does_not_call_answer_model() -> None:
    dataset, prepared = _dataset_and_prepared()
    query = {
        "query_id": "S0:Q2",
        "task": {"sample_index": 0, "question_index": 2},
        "results": [],
    }
    responder = FakeResponder()

    _, answer = PreparedMemoryResponses("raw", responder).answer_question(
        dataset,
        prepared,
        query,
    )

    assert responder.calls == []
    assert answer["response"] == ""


def test_prepared_answer_cache_avoids_duplicate_model_call(tmp_path) -> None:
    dataset, prepared = _dataset_and_prepared()
    responder = FakeResponder()
    cache = JsonCache(tmp_path / "cache", version="answer-test-v1")
    generator = PreparedMemoryResponses(
        "extracted",
        responder=responder,
        cache=cache,
    )

    first = generator.answer_question(dataset, prepared, _extracted_query())
    second = generator.answer_question(dataset, prepared, _extracted_query())

    assert first == second
    assert len(responder.calls) == 1


def test_prepared_generate_preserves_query_order_and_groups_by_sample() -> None:
    dataset, prepared = _dataset_and_prepared()
    responder = FakeResponder()
    generator = PreparedMemoryResponses("raw", responder=responder)
    results = [
        {
            "query_id": "S0:Q1",
            "task": {"sample_index": 0, "question_index": 1},
            "results": [],
        },
        {
            "query_id": "S0:Q0",
            "task": {"sample_index": 0, "question_index": 0},
            "results": [],
        },
    ]

    answers = generator.generate(dataset, prepared, results)

    assert [item["query_id"] for item in answers["0"]] == ["S0:Q1", "S0:Q0"]


def test_response_client_sends_frozen_output_token_limit(monkeypatch) -> None:
    calls = []

    class Completions:
        def create(self, **kwargs):
            calls.append(kwargs)
            return SimpleNamespace(
                choices=[SimpleNamespace(message=SimpleNamespace(content="answer"))],
                usage={"prompt_tokens": 2, "completion_tokens": 1, "total_tokens": 3},
            )

    fake_client = SimpleNamespace(
        chat=SimpleNamespace(completions=Completions())
    )
    monkeypatch.setattr(responses_module, "OpenAI", lambda **kwargs: fake_client)
    monkeypatch.setenv("ANSWER_MAX_TOKENS", "512")

    ResponseClient().answer("a", "b", [], [], "question")

    assert calls[0]["max_tokens"] == 512
