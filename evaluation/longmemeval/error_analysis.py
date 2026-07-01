"""Heuristic error classification for LongMemEval QA results."""

from __future__ import annotations

import re
from collections import defaultdict


def attach_error_analysis(results: list[dict]) -> None:
    for item in results:
        item["error_analysis"] = classify_error(item)


def classify_error(item: dict) -> dict:
    if item.get("correct") is True:
        return {"primary": "correct", "labels": ["correct"]}

    labels = []
    diagnostic = item.get("retrieval_diagnostic") or {}
    gold_hit = diagnostic.get("gold_turn_retrieved")
    question = str(item.get("question", ""))
    answer = str(item.get("generated_answer", ""))
    question_type = str(item.get("question_type", "unknown"))

    if gold_hit is False:
        labels.append("retrieval_miss")
    elif gold_hit is None:
        labels.append("no_gold_reference")

    if _looks_like_abstention(answer):
        labels.append("over_abstention" if gold_hit is True else "abstention")

    if _is_counting_question(question):
        labels.append("counting_error")
    if _is_date_math_question(question):
        labels.append("date_math_error")
    if _is_ordering_question(question):
        labels.append("ordering_error")
    if question_type == "multi-session":
        labels.append("multi_session_reasoning")
    if question_type == "knowledge-update":
        labels.append("knowledge_update_reasoning")

    if gold_hit is True and not labels:
        labels.append("answer_reasoning_error")
    if not labels:
        labels.append("judge_or_answer_format")

    return {
        "primary": _primary_error_label(labels),
        "labels": labels,
    }


def summarize_error_analysis(results: list[dict]) -> dict:
    primary_counts: dict[str, int] = defaultdict(int)
    label_counts: dict[str, int] = defaultdict(int)
    by_type: dict[str, dict[str, int]] = defaultdict(lambda: defaultdict(int))
    examples: dict[str, list[dict]] = defaultdict(list)

    wrong = [item for item in results if item.get("correct") is not True]
    for item in wrong:
        analysis = item.get("error_analysis") or classify_error(item)
        primary = analysis.get("primary", "unknown")
        primary_counts[primary] += 1
        by_type[item.get("question_type", "unknown")][primary] += 1
        for label in analysis.get("labels", []):
            label_counts[label] += 1
        if len(examples[primary]) < 3:
            examples[primary].append(
                {
                    "question_id": item.get("question_id"),
                    "question_type": item.get("question_type", "unknown"),
                    "question": item.get("question", ""),
                    "correct_answer": item.get("correct_answer", ""),
                    "generated_answer": item.get("generated_answer", ""),
                }
            )

    return {
        "num_wrong": len(wrong),
        "primary_counts": dict(sorted(primary_counts.items())),
        "label_counts": dict(sorted(label_counts.items())),
        "by_type": {
            qtype: dict(sorted(counts.items()))
            for qtype, counts in sorted(by_type.items())
        },
        "examples": dict(sorted(examples.items())),
    }


def _looks_like_abstention(text: str) -> bool:
    lowered = text.lower()
    patterns = [
        "information provided is not enough",
        "not enough information",
        "cannot determine",
        "can't determine",
        "not available",
        "not specified",
        "do not have enough",
        "don't have enough",
        "insufficient",
    ]
    return any(pattern in lowered for pattern in patterns)


def _is_counting_question(question: str) -> bool:
    lowered = question.lower()
    if re.search(r"\bhow many (days|weeks|months|years)\b", lowered):
        return False
    return bool(
        re.search(r"\bhow many\b", lowered)
        or re.search(r"\bnumber of\b", lowered)
        or re.search(r"\bcount\b", lowered)
    )


def _is_date_math_question(question: str) -> bool:
    lowered = question.lower()
    return bool(
        re.search(r"\bhow long\b", lowered)
        or re.search(r"\bhow many (days|weeks|months|years)\b", lowered)
        or re.search(r"\b(days|weeks|months|years) (ago|had passed|between)\b", lowered)
        or "what time" in lowered
    )


def _is_ordering_question(question: str) -> bool:
    lowered = question.lower()
    ordering_terms = [
        "first",
        "before",
        "after",
        "earlier",
        "later",
        "most recent",
        "last",
    ]
    return any(term in lowered for term in ordering_terms)


def _primary_error_label(labels: list[str]) -> str:
    priority = [
        "retrieval_miss",
        "no_gold_reference",
        "over_abstention",
        "counting_error",
        "date_math_error",
        "ordering_error",
        "multi_session_reasoning",
        "knowledge_update_reasoning",
        "answer_reasoning_error",
        "abstention",
        "judge_or_answer_format",
    ]
    for label in priority:
        if label in labels:
            return label
    return labels[0] if labels else "unknown"
