"""LongMemEval preprocessor.

Reads a LongMemEval JSON file (array of objects) and produces a single JSON
file conforming to the benchmark-prepared-v1 schema.
"""

import json
import os


def preprocess(input_path: str, output_path: str, max_items: int | None = None) -> str:
    """Preprocess LongMemEval JSON into benchmark-prepared-v1 format.

    Args:
        input_path: Path to LongMemEval JSON file (array of objects).
        output_path: Path where the prepared JSON file will be written.

    Returns:
        The output file path.
    """
    with open(input_path, "r", encoding="utf-8") as f:
        data = json.load(f)
    if max_items is not None:
        data = data[:max_items]

    memories = []
    queries = []
    seen_ids = set()

    for i, item in enumerate(data):
        question_id = str(item.get("question_id", "")).strip()
        if not question_id:
            raise ValueError(f"[{input_path}] item at index {i} is missing question_id")
        if question_id in seen_ids:
            raise ValueError(f"[{input_path}] duplicate question_id: {question_id}")
        seen_ids.add(question_id)
        scope_id = f"lme_{question_id}"
        haystack_session_ids = item.get("haystack_session_ids", [])
        haystack_dates = item.get("haystack_dates", [])
        haystack_sessions = item.get("haystack_sessions", [])
        gold_turn_ids = []

        for session_idx, session in enumerate(haystack_sessions):
            session_id = (
                haystack_session_ids[session_idx]
                if session_idx < len(haystack_session_ids)
                else f"session_{session_idx}"
            )

            for turn_idx, turn in enumerate(session):
                content = turn.get("content", "")
                if not content or not content.strip():
                    continue

                turn_id = f"{question_id}_s{session_idx}_t{turn_idx}"
                if turn.get("has_answer", False):
                    gold_turn_ids.append(turn_id)

                memories.append({
                    "id": turn_id,
                    "text": content,
                    "metadata": {
                        "scope_id": scope_id,
                        "session_id": session_id,
                        "session_date": (
                            haystack_dates[session_idx]
                            if session_idx < len(haystack_dates)
                            else ""
                        ),
                        "session_idx": session_idx,
                        "turn_idx": turn_idx,
                        "has_answer": turn.get("has_answer", False),
                        "question_id": question_id,
                        "role": turn.get("role", ""),
                    },
                })

        queries.append({
            "id": question_id,
            "text": item["question"],
            "filter": {"scope_id": scope_id},
            "metadata": {
                "question_type": item.get("question_type", ""),
                "question_date": item.get("question_date", ""),
                "is_abstention": question_id.endswith("_abs"),
            },
            "task": {
                "type": "open_qa",
                "correct_answer": item.get("answer", ""),
                "gold_session_ids": item.get("answer_session_ids", []),
                "gold_turn_ids": gold_turn_ids,
            },
        })

    output = {
        "schema_version": "benchmark-prepared-v1",
        "dataset": {
            "name": "longmemeval",
            "split": "oracle",
            "source": "xiaowu0162/longmemeval-cleaned",
        },
        "memories": memories,
        "queries": queries,
    }

    os.makedirs(os.path.dirname(output_path), exist_ok=True)

    with open(output_path, "w", encoding="utf-8") as f:
        json.dump(output, f, ensure_ascii=False, indent=2)

    return output_path
