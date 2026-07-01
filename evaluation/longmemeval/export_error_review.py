"""Export LongMemEval QA wrong cases to a Markdown review file."""

from __future__ import annotations

import argparse
import json
import os
import sys
from collections import defaultdict

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from longmemeval.eval_qa import classify_error


ERROR_ORDER = [
    "over_abstention",
    "counting_error",
    "ordering_error",
    "date_math_error",
    "multi_session_reasoning",
    "retrieval_miss",
    "no_gold_reference",
    "answer_reasoning_error",
    "judge_or_answer_format",
]


def export_error_review(
    qa_results_path: str,
    search_results_path: str,
    output_path: str,
    max_evidence: int = 5,
) -> None:
    qa_results = _read_json(qa_results_path)
    search_results = {
        item["query_id"]: item for item in _read_json(search_results_path)
    }

    wrong = []
    for item in qa_results:
        if item.get("correct") is True:
            continue
        analysis = item.get("error_analysis") or classify_error(item)
        wrong.append((analysis.get("primary", "unknown"), analysis, item))

    grouped: dict[str, list[tuple[dict, dict]]] = defaultdict(list)
    for primary, analysis, item in wrong:
        grouped[primary].append((analysis, item))

    lines = [
        "# LongMemEval QA Error Review",
        "",
        f"- QA results: `{qa_results_path}`",
        f"- Search results: `{search_results_path}`",
        f"- Total questions: {len(qa_results)}",
        f"- Wrong questions: {len(wrong)}",
        "",
        "## Summary",
        "",
        "| Error type | Count |",
        "|---|---:|",
    ]

    for label in _ordered_labels(grouped):
        lines.append(f"| `{label}` | {len(grouped[label])} |")

    for label in _ordered_labels(grouped):
        lines.extend(["", f"## {label}", ""])
        for index, (analysis, item) in enumerate(grouped[label], start=1):
            qid = item.get("question_id", "")
            search_item = search_results.get(qid, {})
            lines.extend(
                [
                    f"### {index}. `{qid}`",
                    "",
                    f"- Type: `{item.get('question_type', 'unknown')}`",
                    f"- Labels: `{', '.join(analysis.get('labels', []))}`",
                    f"- Gold hit: `{(item.get('retrieval_diagnostic') or {}).get('gold_turn_retrieved')}`",
                    f"- Judge: `{_one_line(item.get('judge_response', ''))}`",
                    "",
                    "**Question**",
                    "",
                    _quote_block(str(item.get("question", ""))),
                    "",
                    "**Gold answer**",
                    "",
                    _quote_block(str(item.get("correct_answer", ""))),
                    "",
                    "**Generated answer**",
                    "",
                    _quote_block(str(item.get("generated_answer", ""))),
                    "",
                    "**Top evidence**",
                    "",
                ]
            )
            lines.extend(_render_evidence(search_item.get("results", []), max_evidence))
            lines.append("")

    os.makedirs(os.path.dirname(output_path) or ".", exist_ok=True)
    with open(output_path, "w", encoding="utf-8") as f:
        f.write("\n".join(lines).rstrip() + "\n")


def _ordered_labels(grouped: dict[str, list]) -> list[str]:
    known = [label for label in ERROR_ORDER if label in grouped]
    unknown = sorted(label for label in grouped if label not in ERROR_ORDER)
    return known + unknown


def _render_evidence(results: list[dict], max_evidence: int) -> list[str]:
    if not results:
        return ["No retrieved evidence."]
    lines = []
    for index, result in enumerate(results[:max_evidence], start=1):
        meta = result.get("metadata") or {}
        lines.extend(
            [
                f"{index}. `{result.get('id', '')}` "
                f"role=`{meta.get('role', 'unknown')}` "
                f"date=`{meta.get('session_date', 'unknown')}` "
                f"score=`{float(result.get('score', 0.0)):.4f}`",
                "",
                _quote_block(_one_line(result.get("text", ""), limit=700)),
                "",
            ]
        )
    return lines


def _quote_block(text: str) -> str:
    text = text.strip() or "(empty)"
    return "\n".join(f"> {line}" for line in text.splitlines())


def _one_line(value: object, limit: int = 500) -> str:
    text = " ".join(str(value).split())
    if len(text) <= limit:
        return text
    return text[: limit - 3].rstrip() + "..."


def _read_json(path: str) -> object:
    with open(path, "r", encoding="utf-8") as f:
        return json.load(f)


def main() -> None:
    parser = argparse.ArgumentParser(description="Export LongMemEval QA error review")
    parser.add_argument("--qa-results", required=True)
    parser.add_argument("--search-results", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--max-evidence", type=int, default=5)
    args = parser.parse_args()

    export_error_review(
        qa_results_path=args.qa_results,
        search_results_path=args.search_results,
        output_path=args.output,
        max_evidence=args.max_evidence,
    )
    print(f"wrote error review to {args.output}")


if __name__ == "__main__":
    main()
