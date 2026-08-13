"""Tests for LoCoMo answer judging."""

import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from common.llm_client import ChatResult
from common.json_cache import JsonCache
from locomo.locomo_eval import evaluate_llm_judge, load_and_evaluate_locomo, process_item, _parse_label


class FakeJudgeClient:
    def __init__(self, responses):
        self.responses = list(responses)
        self.calls = []

    def chat(self, model, messages, temperature=0.0, max_tokens=512):
        self.calls.append(
            {
                "model": model,
                "messages": messages,
                "temperature": temperature,
                "max_tokens": max_tokens,
            }
        )
        return ChatResult(
            content=self.responses.pop(0),
            latency_ms=5.0,
            prompt_tokens=10,
            completion_tokens=2,
            total_tokens=12,
            raw={"model": model},
        )


def test_parse_label_extracts_json_without_mem0_dependency():
    assert _parse_label('```json\n{"label": "CORRECT"}\n```') == "CORRECT"
    assert _parse_label('Judgment: {"label": "WRONG"}') == "WRONG"
    assert _parse_label("After review the label is CORRECT.") == "CORRECT"


def test_evaluate_llm_judge_uses_openai_compatible_chat_interface():
    judge_client = FakeJudgeClient(['{"label": "CORRECT"}'])

    score = evaluate_llm_judge(
        question="Where did I park?",
        gold_answer="Level 2",
        generated_answer="You parked on Level 2.",
        judge_client=judge_client,
        judge_model="judge-model",
    )

    assert score == 1
    assert judge_client.calls[0]["model"] == "judge-model"
    assert judge_client.calls[0]["temperature"] == 0.0
    assert "Where did I park?" in judge_client.calls[0]["messages"][0]["content"]


def test_process_item_preserves_output_schema_and_skips_category_five():
    judge_client = FakeJudgeClient(['{"label": "WRONG"}'])

    results = process_item(
        (
            "conversation-1",
            [
                {
                    "question": "What tea did I order?",
                    "answer": "oolong",
                    "response": "green tea",
                    "category": "1",
                    "prompt_tokens": 7,
                    "completion_tokens": 3,
                    "total_tokens": 10,
                    "response_time": 1.5,
                },
                {
                    "question": "Unanswerable?",
                    "answer": "not enough information",
                    "response": "not enough information",
                    "category": "5",
                },
            ],
        ),
        judge_client=judge_client,
        judge_model="judge-model",
    )

    scored = results["conversation-1"]
    assert len(scored) == 1
    assert set(scored[0]) == {
        "question",
        "answer",
        "response",
        "category",
        "bleu_score",
        "f1_score",
        "llm_score",
        "prompt_tokens",
        "completion_tokens",
        "total_tokens",
        "response_time",
        "judge_prompt_tokens",
        "judge_completion_tokens",
        "judge_total_tokens",
        "judge_latency_ms",
    }
    assert scored[0]["llm_score"] == 0
    assert scored[0]["judge_total_tokens"] == 12
    assert scored[0]["judge_latency_ms"] == 5.0
    assert judge_client.calls[0]["model"] == "judge-model"


def test_load_and_evaluate_locomo_writes_original_result_shape(tmp_path):
    input_path = tmp_path / "responses.json"
    output_path = tmp_path / "judge_results.json"
    input_path.write_text(
        json.dumps(
            {
                "conversation-1": [
                    {
                        "question": "What did I buy?",
                        "answer": "notebooks",
                        "response": "notebooks",
                        "category": "2",
                    }
                ]
            }
        ),
        encoding="utf-8",
    )

    load_and_evaluate_locomo(
        input_path=input_path,
        output_path=output_path,
        judge_client=FakeJudgeClient(['{"label": "CORRECT"}']),
        judge_model="judge-model",
        max_workers=1,
        show_progress=False,
    )

    saved = json.loads(output_path.read_text(encoding="utf-8"))
    assert list(saved) == ["conversation-1"]
    assert saved["conversation-1"][0]["question"] == "What did I buy?"
    assert saved["conversation-1"][0]["answer"] == "notebooks"
    assert saved["conversation-1"][0]["response"] == "notebooks"
    assert saved["conversation-1"][0]["category"] == "2"
    assert saved["conversation-1"][0]["llm_score"] == 1


def test_judge_cache_reuses_completed_query_and_preserves_identity(tmp_path):
    judge_client = FakeJudgeClient(['{"label": "CORRECT"}'])
    cache = JsonCache(tmp_path / "cache", version="judge-test-v1")
    item = (
        "0",
        [
            {
                "query_id": "S0:Q0",
                "question": "Where?",
                "answer": "There",
                "response": "There",
                "category": 1,
            }
        ],
    )

    first = process_item(
        item,
        judge_client,
        "openai/gpt-4o-mini",
        cache=cache,
    )
    second = process_item(
        item,
        judge_client,
        "openai/gpt-4o-mini",
        cache=cache,
    )

    assert first == second
    assert len(judge_client.calls) == 1
    assert first["0"][0]["query_id"] == "S0:Q0"
