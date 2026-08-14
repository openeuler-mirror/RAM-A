"""Tests for LongMemEval QA evaluation."""

import json
import os
import sys
import tempfile

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from common.llm_client import ChatResult
from longmemeval.eval_qa import (
    classify_error,
    compute_qa_metrics,
    evaluate_qa,
    format_answer_prompt,
    format_memories,
    load_and_evaluate_qa,
    parse_yes_no,
)


class FakeClient:
    def __init__(self, responses):
        self.responses = list(responses)

    def chat(self, model, messages, temperature=0.0, max_tokens=512):
        content = self.responses.pop(0)
        return ChatResult(
            content=content,
            latency_ms=10.0,
            prompt_tokens=100,
            completion_tokens=10,
            total_tokens=110,
            raw={"model": model},
        )


def _prepared():
    return {
        "queries": [
            {
                "id": "q001",
                "text": "What is my commute?",
                "metadata": {"question_type": "single-session-user"},
                "task": {
                    "correct_answer": "45 minutes",
                    "gold_turn_ids": ["q001_s0_t0"],
                },
            },
            {
                "id": "q002_abs",
                "text": "What is my dog name?",
                "metadata": {"question_type": "single-session-user"},
                "task": {"correct_answer": "The information is not available."},
            },
        ]
    }


def _search_results():
    return [
        {
            "query_id": "q001",
            "results": [
                {
                    "id": "q001_s0_t0",
                    "text": "My commute is 45 minutes.",
                    "metadata": {"session_date": "2023/05/15", "role": "user"},
                    "score": 0.9,
                }
            ],
        },
        {"query_id": "q002_abs", "results": []},
    ]


def test_parse_yes_no():
    assert parse_yes_no("yes") is True
    assert parse_yes_no("No.") is False
    assert parse_yes_no("After checking, yes.") is True


def test_evaluate_qa_writes_results_and_metrics():
    with tempfile.TemporaryDirectory() as tmpdir:
        results_path = os.path.join(tmpdir, "qa_results.json")
        metrics_path = os.path.join(tmpdir, "qa_metrics.json")
        metrics = evaluate_qa(
            search_results=_search_results(),
            prepared=_prepared(),
            output_results_path=results_path,
            output_metrics_path=metrics_path,
            answerer_client=FakeClient(["45 minutes", "not enough information"]),
            judge_client=FakeClient(["yes", "yes"]),
            answerer_model="answerer",
            judge_model="judge",
        )

        assert metrics["overall"]["accuracy"] == 1.0
        assert metrics["overall"]["correct"] == 2
        assert os.path.isfile(results_path)
        assert os.path.isfile(metrics_path)

        saved = json.load(open(results_path, encoding="utf-8"))
        assert saved[0]["generated_answer"] == "45 minutes"
        assert saved[0]["answerer"]["total_tokens"] == 110
        assert saved[0]["retrieval_diagnostic"]["gold_turn_retrieved"] is True
        assert saved[0]["retrieval_diagnostic"]["gold_turn_best_rank"] == 1
        assert saved[1]["retrieval_diagnostic"]["gold_turn_retrieved"] is None
        assert metrics["diagnostics"]["correct_gold_hit"] == 1
        assert metrics["diagnostics"]["no_gold_correct"] == 1
        assert metrics["answer_prompt_version"] == "lme_default"


def test_qa_diagnostics_use_expanded_source_turn_ids():
    prepared = {
        "queries": [
            {
                "id": "q001",
                "text": "What is my commute?",
                "metadata": {"question_type": "single-session-user"},
                "task": {
                    "correct_answer": "45 minutes",
                    "gold_turn_ids": ["q001_s0_t0"],
                },
            }
        ]
    }
    search_results = [
        {
            "query_id": "q001",
            "results": [
                {
                    "id": "mem-a",
                    "text": "The commute takes 45 minutes.",
                    "metadata": {
                        "memory_kind": "extracted_memory",
                        "evidence_refs": [
                            {"message_id": "q001_s1_t0"},
                            {"message_id": "q001_s0_t0"},
                        ],
                    },
                    "score": 0.9,
                }
            ],
        }
    ]

    with tempfile.TemporaryDirectory() as tmpdir:
        results_path = os.path.join(tmpdir, "qa_results.json")
        evaluate_qa(
            search_results=search_results,
            prepared=prepared,
            output_results_path=results_path,
            output_metrics_path=os.path.join(tmpdir, "qa_metrics.json"),
            answerer_client=FakeClient(["45 minutes"]),
            judge_client=FakeClient(["yes"]),
            answerer_model="answerer",
            judge_model="judge",
        )

        saved = json.load(open(results_path, encoding="utf-8"))
        diagnostic = saved[0]["retrieval_diagnostic"]
        assert diagnostic["retrieved_source_turn_ids"] == [
            "q001_s1_t0",
            "q001_s0_t0",
        ]
        assert diagnostic["gold_turn_retrieved"] is True
        assert diagnostic["gold_turn_ranks"] == [2]
        assert diagnostic["gold_turn_best_rank"] == 2


def test_lme_default_prompt_records_version_and_rules():
    prompt = format_answer_prompt(
        question="What did I buy after the webinar?",
        question_date="2023/05/15",
        retrieved=_search_results()[0]["results"],
        question_type="temporal-reasoning",
        answer_prompt_version="lme_default",
    )
    assert "LongMemEval" in prompt
    assert "Distinguish planning from completed events" in prompt
    assert "thinking about buying" in prompt

    with tempfile.TemporaryDirectory() as tmpdir:
        metrics = evaluate_qa(
            search_results=_search_results(),
            prepared=_prepared(),
            output_results_path=os.path.join(tmpdir, "qa_results_lme_default.json"),
            output_metrics_path=os.path.join(tmpdir, "qa_metrics_lme_default.json"),
            answerer_client=FakeClient(["45 minutes", "not enough information"]),
            judge_client=FakeClient(["yes", "yes"]),
            answerer_model="answerer",
            judge_model="judge",
            answer_prompt_version="lme_default",
        )
        assert metrics["answer_prompt_version"] == "lme_default"

        saved = json.load(open(os.path.join(tmpdir, "qa_results_lme_default.json"), encoding="utf-8"))
        assert saved[0]["answer_prompt_version"] == "lme_default"


def test_compact_memory_format_records_version_and_simplifies_context():
    prompt = format_answer_prompt(
        question="What is my commute?",
        question_date="2023/05/15",
        retrieved=_search_results()[0]["results"],
        question_type="single-session-user",
        answer_prompt_version="lme_default",
        memory_format="compact",
    )
    assert "[M1]" in prompt
    assert "content: My commute is 45 minutes." in prompt
    assert "score=" not in prompt

    full = format_memories(_search_results()[0]["results"], memory_format="full")
    full_with_scores = format_memories(_search_results()[0]["results"], memory_format="full", show_scores=True)
    compact = format_memories(_search_results()[0]["results"], memory_format="compact")
    assert "score=" not in full
    assert "score=" in full_with_scores
    assert "score=" not in compact

    with tempfile.TemporaryDirectory() as tmpdir:
        metrics = evaluate_qa(
            search_results=_search_results(),
            prepared=_prepared(),
            output_results_path=os.path.join(tmpdir, "qa_results_lme_default_compact.json"),
            output_metrics_path=os.path.join(tmpdir, "qa_metrics_lme_default_compact.json"),
            answerer_client=FakeClient(["45 minutes", "not enough information"]),
            judge_client=FakeClient(["yes", "yes"]),
            answerer_model="answerer",
            judge_model="judge",
            answer_prompt_version="lme_default",
            memory_format="compact",
        )
        assert metrics["memory_format"] == "compact"

        saved = json.load(open(os.path.join(tmpdir, "qa_results_lme_default_compact.json"), encoding="utf-8"))
        assert saved[0]["memory_format"] == "compact"


def test_graph_fact_context_limit_is_explicit_for_full_and_compact_formats():
    retrieved = [
        {
            "id": "q001_s0_t0",
            "text": "I enjoy live music.",
            "metadata": {
                "session_date": "2023/05/15",
                "role": "user",
                "graph_facts": [
                    {
                        "fact_id": "fact-1",
                        "predicate": "LIKES",
                        "fact_text": "The user likes jazz.",
                    }
                ],
            },
            "score": 0.9,
        }
    ]

    control = format_memories(retrieved, max_graph_context_facts=0)
    full = format_memories(retrieved, max_graph_context_facts=3)
    compact = format_memories(
        retrieved,
        memory_format="compact",
        max_graph_context_facts=3,
    )

    expected = "Matched graph facts:\n- [LIKES] The user likes jazz."
    assert expected not in control
    assert expected in full
    assert expected in compact


def test_graph_fact_context_limit_is_global_across_retrieved_memories():
    retrieved = [
        {
            "id": f"memory-{index}",
            "text": f"Memory {index}.",
            "metadata": {
                "graph_facts": [
                    {
                        "fact_id": f"fact-{index}",
                        "predicate": "RELATED_TO",
                        "fact_text": f"Graph fact {index}.",
                    }
                ]
            },
        }
        for index in range(5)
    ]

    rendered = format_memories(retrieved, max_graph_context_facts=3)

    assert rendered.count("\n- [RELATED_TO]") == 3


def test_compute_qa_metrics_by_type():
    metrics = compute_qa_metrics(
        [
            {
                "question_type": "a",
                "correct": True,
                "answerer": {"latency_ms": 1, "total_tokens": 10, "prompt_tokens": 8},
                "judge": {"latency_ms": 2, "total_tokens": 3},
                "retrieval_diagnostic": {"gold_turn_retrieved": True},
            },
            {
                "question_type": "a",
                "correct": False,
                "answerer": {"latency_ms": 3, "total_tokens": 20, "prompt_tokens": 16},
                "judge": {"latency_ms": 4, "total_tokens": 5},
                "retrieval_diagnostic": {"gold_turn_retrieved": False},
            },
        ]
    )
    assert metrics["overall"]["accuracy"] == 0.5
    assert metrics["by_type"]["a"]["avg_total_latency_ms"] == 5.0
    assert metrics["diagnostics"]["gold_hit_rate"] == 0.5
    assert metrics["diagnostics"]["correct_gold_hit"] == 1
    assert metrics["diagnostics"]["wrong_gold_miss"] == 1


def test_compute_qa_metrics_handles_missing_diagnostics():
    metrics = compute_qa_metrics(
        [
            {
                "question_type": "a",
                "correct": False,
                "answerer": {"latency_ms": 1, "total_tokens": 10, "prompt_tokens": 8},
                "judge": {"latency_ms": 2, "total_tokens": 3},
            }
        ]
    )
    assert metrics["diagnostics"]["with_gold_reference"] == 0
    assert metrics["diagnostics"]["without_gold_reference"] == 1
    assert metrics["diagnostics"]["no_gold_wrong"] == 1


def test_error_analysis_classifies_actionable_errors():
    retrieval_miss = classify_error(
        {
            "correct": False,
            "question": "What is my commute?",
            "generated_answer": "45 minutes",
            "question_type": "single-session-user",
            "retrieval_diagnostic": {"gold_turn_retrieved": False},
        }
    )
    assert retrieval_miss["primary"] == "retrieval_miss"

    over_abstention = classify_error(
        {
            "correct": False,
            "question": "Which seeds were started first?",
            "generated_answer": "The information provided is not enough.",
            "question_type": "temporal-reasoning",
            "retrieval_diagnostic": {"gold_turn_retrieved": True},
        }
    )
    assert over_abstention["primary"] == "over_abstention"
    assert "ordering_error" in over_abstention["labels"]

    date_math = classify_error(
        {
            "correct": False,
            "question": "How many days had passed between Holi and Sunday mass?",
            "generated_answer": "28 days",
            "question_type": "temporal-reasoning",
            "retrieval_diagnostic": {"gold_turn_retrieved": True},
        }
    )
    assert date_math["primary"] == "date_math_error"
    assert "counting_error" not in date_math["labels"]

    multi_session = classify_error(
        {
            "correct": False,
            "question": "What did I do after the workshop?",
            "generated_answer": "unknown",
            "question_type": "multi-session",
            "retrieval_diagnostic": {"gold_turn_retrieved": True},
        }
    )
    assert "multi_session_reasoning" in multi_session["labels"]


def test_compute_qa_metrics_includes_error_analysis():
    metrics = compute_qa_metrics(
        [
            {
                "question_id": "q1",
                "question": "Which event happened first?",
                "question_type": "temporal-reasoning",
                "correct": False,
                "generated_answer": "The information provided is not enough.",
                "answerer": {"latency_ms": 1, "total_tokens": 10, "prompt_tokens": 8},
                "judge": {"latency_ms": 2, "total_tokens": 3},
                "retrieval_diagnostic": {"gold_turn_retrieved": True},
            }
        ]
    )
    assert metrics["error_analysis"]["num_wrong"] == 1
    assert metrics["error_analysis"]["primary_counts"]["over_abstention"] == 1
    assert metrics["error_analysis"]["examples"]["over_abstention"][0]["question_id"] == "q1"


def test_load_and_evaluate_resume_complete_results_without_api_key():
    with tempfile.TemporaryDirectory() as tmpdir:
        search_path = os.path.join(tmpdir, "search_results.json")
        prepared_path = os.path.join(tmpdir, "prepared.json")
        results_path = os.path.join(tmpdir, "qa_results.json")
        metrics_path = os.path.join(tmpdir, "qa_metrics.json")

        json.dump(_search_results(), open(search_path, "w", encoding="utf-8"))
        json.dump(_prepared(), open(prepared_path, "w", encoding="utf-8"))
        json.dump(
            [
                {
                    "question_id": "q001",
                    "question_type": "single-session-user",
                    "correct": True,
                    "generated_answer": "45 minutes",
                    "judge_response": "yes",
                    "max_graph_context_facts": 3,
                    "answerer": {"latency_ms": 1, "total_tokens": 10, "prompt_tokens": 8},
                    "judge": {"latency_ms": 2, "total_tokens": 3},
                },
                {
                    "question_id": "q002_abs",
                    "question_type": "single-session-user",
                    "correct": False,
                    "generated_answer": "Not enough information.",
                    "judge_response": "no",
                    "max_graph_context_facts": 3,
                    "answerer": {"latency_ms": 1, "total_tokens": 10, "prompt_tokens": 8},
                    "judge": {"latency_ms": 2, "total_tokens": 3},
                },
            ],
            open(results_path, "w", encoding="utf-8"),
        )

        # Create results with the current prompt hash before checking that a
        # complete run can resume without an API key.
        evaluate_qa(
            search_results=_search_results(),
            prepared=_prepared(),
            output_results_path=results_path,
            output_metrics_path=metrics_path,
            answerer_client=FakeClient(["45 minutes", "Not enough information."]),
            judge_client=FakeClient(["yes", "no"]),
            answerer_model="answerer",
            judge_model="judge",
        )

        old_key = os.environ.pop("OPENROUTER_API_KEY", None)
        try:
            metrics = load_and_evaluate_qa(
                search_results_path=search_path,
                prepared_path=prepared_path,
                output_results_path=results_path,
                output_metrics_path=metrics_path,
                answerer_model="answerer",
                judge_model="judge",
                resume=True,
            )
        finally:
            if old_key is not None:
                os.environ["OPENROUTER_API_KEY"] = old_key

        assert metrics["diagnostics"]["correct_gold_hit"] == 1
        saved = json.load(open(results_path, encoding="utf-8"))
        assert saved[0]["retrieval_diagnostic"]["gold_turn_retrieved"] is True


def test_load_and_evaluate_resume_ignores_incomplete_results():
    with tempfile.TemporaryDirectory() as tmpdir:
        results_path = os.path.join(tmpdir, "qa_results.json")
        metrics_path = os.path.join(tmpdir, "qa_metrics.json")

        json.dump(
            [
                {
                    "question_id": "q001",
                    "question_type": "single-session-user",
                    "correct": False,
                    "generated_answer": "",
                    "judge_response": "",
                }
            ],
            open(results_path, "w", encoding="utf-8"),
        )

        metrics = evaluate_qa(
            search_results=_search_results()[:1],
            prepared={"queries": _prepared()["queries"][:1]},
            output_results_path=results_path,
            output_metrics_path=metrics_path,
            answerer_client=FakeClient(["45 minutes"]),
            judge_client=FakeClient(["yes"]),
            answerer_model="answerer",
            judge_model="judge",
            resume=True,
        )

        saved = json.load(open(results_path, encoding="utf-8"))
        assert len(saved) == 1
        assert saved[0]["generated_answer"] == "45 minutes"
        assert saved[0]["judge_response"] == "yes"
        assert metrics["overall"]["accuracy"] == 1.0


def test_load_and_evaluate_resume_rejects_prompt_version_mismatch_without_api_key():
    with tempfile.TemporaryDirectory() as tmpdir:
        search_path = os.path.join(tmpdir, "search_results.json")
        prepared_path = os.path.join(tmpdir, "prepared.json")
        results_path = os.path.join(tmpdir, "qa_results_lme_default.json")
        metrics_path = os.path.join(tmpdir, "qa_metrics_lme_default.json")

        json.dump(_search_results(), open(search_path, "w", encoding="utf-8"))
        json.dump(_prepared(), open(prepared_path, "w", encoding="utf-8"))
        json.dump(
            [
                {
                    "question_id": "q001",
                    "question_type": "single-session-user",
                    "correct": True,
                    "answer_prompt_version": "old_prompt",
                    "answerer": {"latency_ms": 1, "total_tokens": 10, "prompt_tokens": 8},
                    "judge": {"latency_ms": 2, "total_tokens": 3},
                },
                {
                    "question_id": "q002_abs",
                    "question_type": "single-session-user",
                    "correct": False,
                    "answer_prompt_version": "old_prompt",
                    "answerer": {"latency_ms": 1, "total_tokens": 10, "prompt_tokens": 8},
                    "judge": {"latency_ms": 2, "total_tokens": 3},
                },
            ],
            open(results_path, "w", encoding="utf-8"),
        )

        old_key = os.environ.pop("OPENROUTER_API_KEY", None)
        try:
            try:
                load_and_evaluate_qa(
                    search_results_path=search_path,
                    prepared_path=prepared_path,
                    output_results_path=results_path,
                    output_metrics_path=metrics_path,
                    answerer_model="answerer",
                    judge_model="judge",
                    resume=True,
                    answer_prompt_version="lme_default",
                )
            except RuntimeError as error:
                assert "missing API key env" in str(error)
            else:
                raise AssertionError("expected missing API key error")
        finally:
            if old_key is not None:
                os.environ["OPENROUTER_API_KEY"] = old_key


def main():
    tests = [
        test_parse_yes_no,
        test_evaluate_qa_writes_results_and_metrics,
        test_lme_default_prompt_records_version_and_rules,
        test_compact_memory_format_records_version_and_simplifies_context,
        test_compute_qa_metrics_by_type,
        test_compute_qa_metrics_handles_missing_diagnostics,
        test_error_analysis_classifies_actionable_errors,
        test_compute_qa_metrics_includes_error_analysis,
        test_load_and_evaluate_resume_complete_results_without_api_key,
        test_load_and_evaluate_resume_ignores_incomplete_results,
        test_load_and_evaluate_resume_rejects_prompt_version_mismatch_without_api_key,
    ]
    for test_fn in tests:
        print(f"  {test_fn.__name__}...", end=" ", flush=True)
        test_fn()
        print("OK")
    print("all eval_qa tests passed")


if __name__ == "__main__":
    main()
