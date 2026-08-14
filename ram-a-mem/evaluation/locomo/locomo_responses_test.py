from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace

from common.json_cache import JsonCache
from locomo.locomo_adapter import prepare_locomo
import locomo.locomo_responses as responses_module
from locomo.locomo_responses import (
    PreparedMemoryResponses,
    ResponseClient,
    format_context_groups,
)


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


def test_prepared_answer_applies_explicit_graph_fact_limit() -> None:
    dataset, prepared = _dataset_and_prepared()
    query = _extracted_query()
    query["results"][0]["metadata"]["graph_facts"] = [
        {
            "fact_id": "fact-1",
            "predicate": "RELATED_TO",
            "fact_text": "Alex left a notebook on the table.",
        }
    ]

    control_responder = FakeResponder()
    PreparedMemoryResponses(
        "extracted",
        responder=control_responder,
        max_graph_context_facts=0,
    ).answer_question(dataset, prepared, query)
    treatment_responder = FakeResponder()
    PreparedMemoryResponses(
        "extracted",
        responder=treatment_responder,
        max_graph_context_facts=3,
    ).answer_question(dataset, prepared, query)

    control_context = "\n".join(control_responder.calls[0][2])
    treatment_context = "\n".join(treatment_responder.calls[0][2])
    assert "Matched graph facts" not in control_context
    assert "[RELATED_TO] Alex left a notebook on the table." in treatment_context


def test_locomo_speaker_groups_share_one_graph_fact_budget():
    contexts = {
        "Alex": [
            {
                "memory": "Alex: First.",
                "timestamp": "day 1",
                "score": 0.9,
                "graph_facts": [{"fact_id": "fact-1", "fact_text": "Fact one."}],
            }
        ],
        "Blair": [
            {
                "memory": "Blair: Second.",
                "timestamp": "day 2",
                "score": 0.8,
                "graph_facts": [{"fact_id": "fact-2", "fact_text": "Fact two."}],
            }
        ],
    }

    rendered = format_context_groups(contexts, ("Alex", "Blair"), 1)

    assert sum(text.count("\n- ") for values in rendered.values() for text in values) == 1
    assert "Fact one." in rendered["Alex"][0]
    assert "Fact two." not in rendered["Blair"][0]


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


def test_prepared_answer_cache_separates_graph_fact_limits(tmp_path) -> None:
    dataset, prepared = _dataset_and_prepared()
    responder = FakeResponder()
    cache = JsonCache(tmp_path / "cache", version="answer-test-v1")

    PreparedMemoryResponses(
        "extracted",
        responder=responder,
        cache=cache,
        max_graph_context_facts=0,
    ).answer_question(dataset, prepared, _extracted_query())
    PreparedMemoryResponses(
        "extracted",
        responder=responder,
        cache=cache,
        max_graph_context_facts=3,
    ).answer_question(dataset, prepared, _extracted_query())

    assert len(responder.calls) == 2


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
