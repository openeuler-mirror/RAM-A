"""Tests for eval_retrieval."""

import json
import os
import sys
import tempfile

import pytest

# Allow running from any directory
sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from longmemeval.eval_retrieval import evaluate_retrieval, load_and_evaluate


def _make_search_results():
    """Return search results in the prepared-schema QueryOutput format."""
    return [
        {
            "query_path": "$.queries[0].text",
            "query": "How long is my commute?",
            "query_id": "q001",
            "filter": {"scope_id": "lme_user_0"},
            "metadata": {"question_type": "single-session-user"},
            "task": {"type": "open_qa", "correct_answer": "45 minutes"},
            "results": [
                {
                    "id": "q001_s0_t0",
                    "text": "My commute is 45 minutes each way",
                    "metadata": {
                        "scope_id": "lme_user_0",
                        "session_id": "session_0",
                        "has_answer": True,
                    },
                    "score": 0.95,
                },
                {
                    "id": "q001_s0_t1",
                    "text": "That is not too bad for a daily commute.",
                    "metadata": {
                        "scope_id": "lme_user_0",
                        "session_id": "session_0",
                        "has_answer": False,
                    },
                    "score": 0.80,
                },
                {
                    "id": "q001_s1_t0",
                    "text": "I like trains",
                    "metadata": {
                        "scope_id": "lme_user_0",
                        "session_id": "session_1",
                        "has_answer": False,
                    },
                    "score": 0.50,
                },
            ],
        },
        {
            "query_path": "$.queries[1].text",
            "query": "What is my dog's name?",
            "query_id": "q002",
            "filter": {"scope_id": "lme_user_1"},
            "metadata": {"question_type": "multi-session"},
            "task": {"type": "open_qa", "correct_answer": "Rex"},
            "results": [
                {
                    "id": "q002_s1_t0",
                    "text": "I like cats",
                    "metadata": {
                        "scope_id": "lme_user_1",
                        "session_id": "session_1",
                        "has_answer": False,
                    },
                    "score": 0.60,
                },
                {
                    "id": "q002_s2_t0",
                    "text": "My cat is fluffy",
                    "metadata": {
                        "scope_id": "lme_user_1",
                        "session_id": "session_2",
                        "has_answer": False,
                    },
                    "score": 0.40,
                },
            ],
        },
    ]


def _make_lme_data():
    """Return LongMemEval ground truth."""
    return [
        {
            "question_id": "q001",
            "question_type": "single-session-user",
            "answer_session_ids": ["session_0"],
        },
        {
            "question_id": "q002",
            "question_type": "multi-session",
            "answer_session_ids": ["session_3", "session_4"],
        },
    ]


def _source_turn_metadata():
    return {
        "q001_s0_t0": {"session_id": "session_0"},
        "q001_s0_t1": {"session_id": "session_0"},
        "q001_s1_t0": {"session_id": "session_1"},
        "q002_s1_t0": {"session_id": "session_1"},
        "q002_s2_t0": {"session_id": "session_2"},
    }


def test_evaluate_session_recall():
    """Session-level recall: q001 finds session_0 (recall=1.0), q002 misses
    session_3/4 (recall=0.0).  Mean recall@10 = 0.5."""
    report = evaluate_retrieval(
        _make_search_results(), _make_lme_data(), _source_turn_metadata()
    )

    session = report["session"]
    assert "overall" in session
    overall = session["overall"]

    assert abs(overall["recall@10"] - 0.5) < 1e-9, (
        f"Expected recall@10=0.5, got {overall['recall@10']}"
    )
    # q001 session_0 is at rank 1 -> mrr = 1.0; q002 -> 0.0; mean = 0.5
    assert abs(overall["mrr"] - 0.5) < 1e-9, (
        f"Expected mrr=0.5, got {overall['mrr']}"
    )

    # By-type checks
    by_type = session["by_type"]
    assert "single-session-user" in by_type
    assert abs(by_type["single-session-user"]["recall@1"] - 1.0) < 1e-9
    assert "multi-session" in by_type
    assert abs(by_type["multi-session"]["recall@10"] - 0.0) < 1e-9


def test_evaluate_turn_recall():
    """Turn-level: q001 has one has_answer=True turn at rank 1.
    q002 has zero has_answer turns."""
    report = evaluate_retrieval(
        _make_search_results(), _make_lme_data(), _source_turn_metadata()
    )

    turn = report["turn"]
    overall = turn["overall"]

    # q001: relevant turn at rank 1 -> recall@1=1.0, mrr=1.0
    # q002: no relevant turns -> recall@1=0.0, mrr=0.0
    # mean recall@1 = 0.5, mean mrr = 0.5
    assert abs(overall["recall@1"] - 0.5) < 1e-9, (
        f"Expected turn recall@1=0.5, got {overall['recall@1']}"
    )
    assert abs(overall["mrr"] - 0.5) < 1e-9, (
        f"Expected turn mrr=0.5, got {overall['mrr']}"
    )

    # nDCG@5: q001 with 1 relevant at rank 1 = 1.0; q002 = 0.0; mean = 0.5
    assert abs(overall["ndcg@5"] - 0.5) < 1e-9, (
        f"Expected turn ndcg@5=0.5, got {overall['ndcg@5']}"
    )


def test_turn_gold_comes_from_dataset_not_retrieved_results():
    """A missed answer turn must score 0 even when it never appears in results."""
    sr = [
        {
            "query_id": "q001",
            "metadata": {"question_type": "single-session-user"},
            "results": [
                {
                    "id": "q001_s0_t1",
                    "metadata": {"session_id": "session_0", "has_answer": False},
                    "score": 0.9,
                }
            ],
        }
    ]
    lme = [
        {
            "question_id": "q001",
            "question_type": "single-session-user",
            "answer_session_ids": ["session_0"],
            "haystack_sessions": [
                [
                    {"role": "user", "content": "gold answer", "has_answer": True},
                    {"role": "assistant", "content": "not gold", "has_answer": False},
                ]
            ],
        }
    ]

    report = evaluate_retrieval(
        sr, lme, {"q001_s0_t1": {"session_id": "session_0"}}
    )

    assert report["session"]["overall"]["recall@1"] == 1.0
    assert report["turn"]["overall"]["recall@1"] == 0.0
    assert report["turn"]["overall"]["mrr"] == 0.0


def test_missing_search_result_counts_as_zero():
    report = evaluate_retrieval([], _make_lme_data(), _source_turn_metadata())

    assert report["num_missing_results"] == 2
    assert report["session"]["overall"]["recall@10"] == 0.0
    assert report["turn"]["overall"]["recall@10"] == 0.0


def test_expected_query_ids_limit_evaluation_scope():
    report = evaluate_retrieval(
        _make_search_results()[:1],
        _make_lme_data(),
        _source_turn_metadata(),
        expected_query_ids=["q001"],
    )

    assert report["num_questions"] == 1
    assert report["num_missing_results"] == 0
    assert report["session"]["overall"]["recall@10"] == 1.0


def test_abstention_excluded():
    """Abstention questions (id ending with _abs) must be excluded."""
    sr = _make_search_results()
    lme = _make_lme_data()
    # Add an abstention question in the new format
    sr.append(
        {
            "query_id": "q003_abs",
            "query": "Unknown topic",
            "filter": {"scope_id": "lme_user_2"},
            "metadata": {"question_type": "abstention"},
            "task": {"type": "open_qa", "correct_answer": ""},
            "results": [],
        }
    )
    lme.append(
        {
            "question_id": "q003_abs",
            "question_type": "abstention",
            "answer_session_ids": [],
        }
    )

    report = evaluate_retrieval(sr, lme, _source_turn_metadata())

    # Only q001 and q002 are non-abstention
    assert report["num_questions"] == 2, (
        f"Expected num_questions=2, got {report['num_questions']}"
    )
    assert report["num_abstention_excluded"] == 1
    # The abstention type should not appear in by_type
    assert "abstention" not in report["session"]["by_type"], (
        "abstention should be excluded from by_type"
    )


def test_load_and_evaluate_writes_file():
    """load_and_evaluate writes a valid JSON metrics file."""
    sr = _make_search_results()
    lme = _make_lme_data()

    with tempfile.TemporaryDirectory() as tmpdir:
        sr_path = os.path.join(tmpdir, "search_results.json")
        lme_path = os.path.join(tmpdir, "lme_data.json")
        prepared_path = os.path.join(tmpdir, "prepared.json")
        out_path = os.path.join(tmpdir, "metrics.json")

        with open(sr_path, "w") as f:
            json.dump(sr, f)
        with open(lme_path, "w") as f:
            json.dump(lme, f)
        with open(prepared_path, "w") as f:
            json.dump(
                {
                    "queries": [{"id": "q001"}, {"id": "q002"}],
                    "memories": [
                        {"id": memory_id, "metadata": metadata}
                        for memory_id, metadata in _source_turn_metadata().items()
                    ],
                },
                f,
            )

        report = load_and_evaluate(
            sr_path, lme_path, out_path, prepared_path=prepared_path
        )

        assert os.path.isfile(out_path)
        with open(out_path) as f:
            from_disk = json.load(f)
        assert from_disk["num_questions"] == report["num_questions"]


def test_question_type_from_search_result_metadata():
    """When search result has metadata.question_type, it should be used
    even if lme_data has a different value."""
    sr = _make_search_results()
    lme = _make_lme_data()

    # Override question_type in search result metadata
    sr[0]["metadata"]["question_type"] = "custom-type"

    report = evaluate_retrieval(sr, lme, _source_turn_metadata())

    by_type = report["session"]["by_type"]
    assert "custom-type" in by_type, (
        "Expected question_type from search result metadata to be used"
    )
    assert "single-session-user" not in by_type, (
        "Old question_type from lme_data should not appear"
    )


def test_extracted_results_score_expanded_source_turns_and_sessions():
    search_results = [
        {
            "query_id": "q001",
            "metadata": {"question_type": "single-session-user"},
            "task": {
                "gold_session_ids": ["session_0"],
                "gold_turn_ids": ["q001_s0_t0"],
            },
            "results": [
                {
                    "id": "mem-a",
                    "metadata": {
                        "memory_kind": "extracted_memory",
                        "session_id": "must-not-be-used",
                        "evidence_refs": [
                            {"message_id": "q001_s1_t0"},
                            {"message_id": "q001_s0_t0"},
                        ],
                    },
                }
            ],
        }
    ]
    lme_data = [
        {
            "question_id": "q001",
            "question_type": "single-session-user",
            "answer_session_ids": ["session_0"],
        }
    ]
    source_turn_metadata = {
        "q001_s1_t0": {"session_id": "session_1"},
        "q001_s0_t0": {"session_id": "session_0"},
    }

    report = evaluate_retrieval(
        search_results, lme_data, source_turn_metadata, ks=[1, 2]
    )

    assert report["turn"]["overall"]["recall@1"] == 0.0
    assert report["turn"]["overall"]["recall@2"] == 1.0
    assert report["turn"]["overall"]["mrr"] == 0.5
    assert report["session"]["overall"]["recall@1"] == 0.0
    assert report["session"]["overall"]["recall@2"] == 1.0


def test_evaluate_retrieval_rejects_missing_source_turn_metadata():
    search_results = [
        {
            "query_id": "q001",
            "results": [
                {
                    "id": "mem-a",
                    "metadata": {
                        "evidence_refs": [{"message_id": "q001_s0_t0"}]
                    },
                }
            ],
        }
    ]

    with pytest.raises(ValueError, match="missing source turn metadata.*q001_s0_t0"):
        evaluate_retrieval(search_results, _make_lme_data()[:1], {})


def main():
    tests = [
        test_evaluate_session_recall,
        test_evaluate_turn_recall,
        test_turn_gold_comes_from_dataset_not_retrieved_results,
        test_missing_search_result_counts_as_zero,
        test_expected_query_ids_limit_evaluation_scope,
        test_abstention_excluded,
        test_load_and_evaluate_writes_file,
        test_question_type_from_search_result_metadata,
    ]
    for test_fn in tests:
        print(f"  {test_fn.__name__}...", end=" ", flush=True)
        try:
            test_fn()
            print("OK")
        except Exception as e:
            print(f"FAILED\n    {e}")
            raise
    print("all eval_retrieval tests passed")


if __name__ == "__main__":
    main()
