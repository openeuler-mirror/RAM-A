"""Tests for LongMemEval preprocessor."""

import json
import os
import sys
import tempfile

# Allow running from any directory
sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from longmemeval.preprocess import preprocess


def _write_sample_input(path: str) -> dict:
    """Write a sample LongMemEval JSON file and return the data."""
    data = [
        {
            "question_id": "q001",
            "question_type": "single-session-user",
            "question": "How long is my commute?",
            "answer": "45 minutes each way",
            "question_date": "2023/05/30 (Tue) 20:36",
            "haystack_session_ids": ["session_0"],
            "haystack_dates": ["2023/05/15"],
            "haystack_sessions": [
                [
                    {"role": "user", "content": "My commute is 45 minutes each way", "has_answer": True},
                    {"role": "assistant", "content": "That is not too bad for a daily commute."},
                ]
            ],
            "answer_session_ids": ["session_0"],
        },
        {
            "question_id": "q002_abs",
            "question_type": "abstention",
            "question": "What is my dog's name?",
            "answer": "I do not have enough information",
            "question_date": "2023/06/01 (Thu) 10:00",
            "haystack_session_ids": ["session_1"],
            "haystack_dates": ["2023/05/20"],
            "haystack_sessions": [
                [
                    {"role": "user", "content": "I just adopted a cat named Whiskers."},
                ]
            ],
            "answer_session_ids": ["session_1"],
        },
    ]
    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f)
    return data


def test_preprocess_outputs_json_file():
    """Verify output file exists and is valid JSON."""
    with tempfile.TemporaryDirectory() as tmpdir:
        input_path = os.path.join(tmpdir, "input.json")
        output_path = os.path.join(tmpdir, "output", "prepared.json")
        _write_sample_input(input_path)

        result = preprocess(input_path, output_path)

        assert result == output_path
        assert os.path.isfile(output_path), f"Output file not found at {output_path}"

        with open(output_path, "r", encoding="utf-8") as f:
            data = json.load(f)

        assert isinstance(data, dict)


def test_preprocess_schema_version():
    """Verify schema_version is benchmark-prepared-v1."""
    with tempfile.TemporaryDirectory() as tmpdir:
        input_path = os.path.join(tmpdir, "input.json")
        output_path = os.path.join(tmpdir, "output", "prepared.json")
        _write_sample_input(input_path)

        preprocess(input_path, output_path)

        with open(output_path, "r", encoding="utf-8") as f:
            data = json.load(f)

        assert data["schema_version"] == "benchmark-prepared-v1"
        assert data["dataset"]["name"] == "longmemeval"
        assert data["dataset"]["split"] == "oracle"
        assert data["dataset"]["source"] == "xiaowu0162/longmemeval-cleaned"


def test_preprocess_memories_content():
    """Verify 3 memories with correct fields and metadata.scope_id."""
    with tempfile.TemporaryDirectory() as tmpdir:
        input_path = os.path.join(tmpdir, "input.json")
        output_path = os.path.join(tmpdir, "output", "prepared.json")
        _write_sample_input(input_path)

        preprocess(input_path, output_path)

        with open(output_path, "r", encoding="utf-8") as f:
            data = json.load(f)

        memories = data["memories"]

        # 2 turns from q001 + 1 turn from q002_abs = 3 total
        assert len(memories) == 3, f"Expected 3 memories, got {len(memories)}"

        # First memory: q001, session 0, turn 0 (user with has_answer=True)
        m0 = memories[0]
        assert m0["id"] == "q001_s0_t0", f"Unexpected id: {m0['id']}"
        assert m0["text"] == "My commute is 45 minutes each way"
        assert m0["metadata"]["scope_id"] == "lme_q001"
        assert m0["metadata"]["session_id"] == "session_0"
        assert m0["metadata"]["session_date"] == "2023/05/15"
        assert m0["metadata"]["has_answer"] is True
        assert m0["metadata"]["role"] == "user"
        assert m0["metadata"]["question_id"] == "q001"
        assert m0["metadata"]["session_idx"] == 0
        assert m0["metadata"]["turn_idx"] == 0

        # Second memory: q001, session 0, turn 1 (assistant, has_answer defaults to False)
        m1 = memories[1]
        assert m1["id"] == "q001_s0_t1", f"Unexpected id: {m1['id']}"
        assert m1["metadata"]["has_answer"] is False
        assert m1["metadata"]["role"] == "assistant"

        # Third memory: q002_abs, session 0, turn 0
        m2 = memories[2]
        assert m2["id"] == "q002_abs_s0_t0", f"Unexpected id: {m2['id']}"
        assert m2["metadata"]["scope_id"] == "lme_q002_abs"
        assert m2["metadata"]["question_id"] == "q002_abs"


def test_preprocess_copies_turn_role_to_speaker_metadata():
    """Ensure atomic extraction receives the turn's deterministic speaker."""
    with tempfile.TemporaryDirectory() as tmpdir:
        input_path = os.path.join(tmpdir, "input.json")
        output_path = os.path.join(tmpdir, "output", "prepared.json")
        _write_sample_input(input_path)

        preprocess(input_path, output_path)

        with open(output_path, "r", encoding="utf-8") as f:
            data = json.load(f)

        assert data["memories"][0]["metadata"]["speaker"] == "user"
        assert data["memories"][1]["metadata"]["speaker"] == "assistant"


def test_preprocess_queries_content():
    """Verify 2 queries with filter.scope_id and task.correct_answer."""
    with tempfile.TemporaryDirectory() as tmpdir:
        input_path = os.path.join(tmpdir, "input.json")
        output_path = os.path.join(tmpdir, "output", "prepared.json")
        _write_sample_input(input_path)

        preprocess(input_path, output_path)

        with open(output_path, "r", encoding="utf-8") as f:
            data = json.load(f)

        queries = data["queries"]
        assert len(queries) == 2, f"Expected 2 queries, got {len(queries)}"

        # First query
        q0 = queries[0]
        assert q0["id"] == "q001"
        assert q0["text"] == "How long is my commute?"
        assert q0["filter"]["scope_id"] == "lme_q001"
        assert q0["metadata"]["question_type"] == "single-session-user"
        assert q0["metadata"]["question_date"] == "2023/05/30 (Tue) 20:36"
        assert q0["task"]["type"] == "open_qa"
        assert q0["task"]["correct_answer"] == "45 minutes each way"
        assert q0["task"]["gold_session_ids"] == ["session_0"]
        assert q0["task"]["gold_turn_ids"] == ["q001_s0_t0"]
        assert q0["metadata"]["is_abstention"] is False

        # Second query (abstention question is still included)
        q1 = queries[1]
        assert q1["id"] == "q002_abs"
        assert q1["text"] == "What is my dog's name?"
        assert q1["filter"]["scope_id"] == "lme_q002_abs"
        assert q1["metadata"]["question_type"] == "abstention"
        assert q1["task"]["correct_answer"] == "I do not have enough information"
        assert q1["metadata"]["is_abstention"] is True


def test_preprocess_skips_empty_content():
    """Verify turns with empty or whitespace-only content are skipped."""
    with tempfile.TemporaryDirectory() as tmpdir:
        input_path = os.path.join(tmpdir, "input.json")
        output_path = os.path.join(tmpdir, "output", "prepared.json")
        data = [
            {
                "question_id": "q003",
                "question_type": "test",
                "question": "Test question",
                "answer": "test answer",
                "question_date": "2023/01/01",
                "haystack_session_ids": ["session_0"],
                "haystack_dates": ["2023/01/01"],
                "haystack_sessions": [
                    [
                        {"role": "user", "content": "Hello"},
                        {"role": "assistant", "content": ""},
                        {"role": "user", "content": "   "},
                        {"role": "assistant", "content": "World"},
                    ]
                ],
                "answer_session_ids": ["session_0"],
            }
        ]
        with open(input_path, "w", encoding="utf-8") as f:
            json.dump(data, f)

        preprocess(input_path, output_path)

        with open(output_path, "r", encoding="utf-8") as f:
            result = json.load(f)

        memories = result["memories"]

        # Only "Hello" and "World" survive; empty string and whitespace are skipped
        assert len(memories) == 2
        assert memories[0]["text"] == "Hello"
        assert memories[0]["id"] == "q003_s0_t0"
        assert memories[1]["text"] == "World"
        assert memories[1]["id"] == "q003_s0_t3"


def main():
    tests = [
        test_preprocess_outputs_json_file,
        test_preprocess_schema_version,
        test_preprocess_memories_content,
        test_preprocess_copies_turn_role_to_speaker_metadata,
        test_preprocess_queries_content,
        test_preprocess_skips_empty_content,
    ]
    for test_fn in tests:
        print(f"  {test_fn.__name__}...", end=" ", flush=True)
        try:
            test_fn()
            print("OK")
        except Exception as e:
            print(f"FAILED\n    {e}")
            raise
    print("all preprocess tests passed")


if __name__ == "__main__":
    main()
