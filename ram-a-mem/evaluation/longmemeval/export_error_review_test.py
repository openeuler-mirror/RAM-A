"""Tests for LongMemEval error review export."""

import json
import os
import sys
import tempfile

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from longmemeval.export_error_review import export_error_review


def test_export_error_review_writes_markdown():
    with tempfile.TemporaryDirectory() as tmpdir:
        qa_path = os.path.join(tmpdir, "qa_results.json")
        search_path = os.path.join(tmpdir, "search_results.json")
        output_path = os.path.join(tmpdir, "review.md")

        with open(qa_path, "w", encoding="utf-8") as f:
            json.dump(
                [
                    {
                        "question_id": "q1",
                        "question": "Which happened first?",
                        "question_type": "temporal-reasoning",
                        "correct_answer": "A",
                        "generated_answer": "The information provided is not enough.",
                        "judge_response": "no",
                        "correct": False,
                        "retrieval_diagnostic": {"gold_turn_retrieved": True},
                    }
                ],
                f,
            )

        with open(search_path, "w", encoding="utf-8") as f:
            json.dump(
                [
                    {
                        "query_id": "q1",
                        "results": [
                            {
                                "id": "m1",
                                "text": "A happened before B.",
                                "score": 0.9,
                                "metadata": {
                                    "role": "user",
                                    "session_date": "2023/01/01",
                                },
                            }
                        ],
                    }
                ],
                f,
            )

        export_error_review(qa_path, search_path, output_path, max_evidence=1)

        text = open(output_path, encoding="utf-8").read()
        assert "# LongMemEval QA Error Review" in text
        assert "## over_abstention" in text
        assert "`q1`" in text
        assert "A happened before B." in text


def main():
    tests = [test_export_error_review_writes_markdown]
    for test_fn in tests:
        print(f"  {test_fn.__name__}...", end=" ", flush=True)
        test_fn()
        print("OK")
    print("all export_error_review tests passed")


if __name__ == "__main__":
    main()
