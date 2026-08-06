"""Prepare the checked-in PersonaMem fixture for graph-aware smoke tests."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from personalmem.run import build_prepared_schema_v1


DEFAULT_SCOPE_ID = "personalmem-sample"


def build_fixture_legacy(sample: dict[str, Any], scope_id: str) -> dict[str, Any]:
    """Convert the raw fixture to the small legacy shape expected by the adapter."""
    conversation = sample.get("conversation")
    questions = sample.get("questions")
    if not isinstance(conversation, list) or not isinstance(questions, list):
        raise ValueError("PersonaMem fixture must contain conversation and questions arrays")

    return {
        "source": "bowen-upenn/PersonaMem",
        "conversation": [
            {
                "id": f"{scope_id}:{index}",
                "shared_context_id": scope_id,
                "speaker": message["speaker"],
                "text": message["text"],
            }
            for index, message in enumerate(conversation)
        ],
        "questions": [
            {
                "question_id": f"personalmem-q-{index}",
                "shared_context_id": scope_id,
                "question_type": "persona",
                "topic": "preference",
                "question": question["question"],
                "answer": question["answer"],
                "correct_answer": "(a)",
                "all_options": [
                    f"(a) {question['answer']}",
                    "(b) None of the above.",
                ],
            }
            for index, question in enumerate(questions)
        ],
    }


def prepare_fixture(input_path: Path, output_path: Path, scope_id: str = DEFAULT_SCOPE_ID) -> int:
    sample = json.loads(input_path.read_text(encoding="utf-8"))
    if not isinstance(sample, dict):
        raise ValueError("PersonaMem fixture must be a JSON object")
    prepared = build_prepared_schema_v1(build_fixture_legacy(sample, scope_id), "fixture")
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(
        json.dumps(prepared, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    return len(prepared["queries"])


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Prepare the checked-in PersonaMem fixture for graph-aware smoke tests."
    )
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--scope-id", default=DEFAULT_SCOPE_ID)
    args = parser.parse_args()
    count = prepare_fixture(args.input, args.output, args.scope_id)
    print(f"prepared PersonaMem fixture: queries={count} output={args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
