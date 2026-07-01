"""LongMemEval end-to-end QA evaluation.

This layer uses retrieved memories to generate an answer, then an LLM judge
decides whether the answer matches the reference. Retrieval metrics remain
diagnostic; QA accuracy is the headline metric.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import time
from collections import defaultdict
from typing import Any

from tqdm import tqdm

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from common.llm_client import OpenAICompatibleClient
from longmemeval.error_analysis import (  # noqa: E402
    attach_error_analysis,
    classify_error,
    summarize_error_analysis as _summarize_error_analysis,
)
from longmemeval.prompts import (  # noqa: E402
    ANSWER_PROMPT_VERSION_DEFAULT,
    ANSWER_PROMPT_VERSIONS,
    ANSWER_SYSTEM,
    MEMORY_FORMAT_DEFAULT,
    MEMORY_FORMATS,
    format_answer_prompt,
    format_memories,
    get_judge_prompt,
    validate_answer_prompt_version as _validate_answer_prompt_version,
    validate_memory_format as _validate_memory_format,
)


class IncompleteResultsError(RuntimeError):
    """Raised when a resume-only QA path needs to generate missing results."""


def evaluate_qa(
    search_results: list[dict],
    prepared: dict,
    output_results_path: str,
    output_metrics_path: str,
    answerer_client: Any,
    judge_client: Any,
    answerer_model: str,
    judge_model: str,
    qa_top_k: int = 10,
    resume: bool = False,
    answer_prompt_version: str = ANSWER_PROMPT_VERSION_DEFAULT,
    memory_format: str = MEMORY_FORMAT_DEFAULT,
    show_progress: bool = False,
    show_scores: bool = False,
) -> dict:
    _validate_answer_prompt_version(answer_prompt_version)
    _validate_memory_format(memory_format)
    existing = _load_existing_results(output_results_path) if resume else {}
    results_by_id = {item["query_id"]: item for item in search_results}
    queries = prepared.get("queries", [])
    started = time.monotonic()
    expected_config = {
        "answerer_model": answerer_model,
        "judge_model": judge_model,
        "qa_top_k": qa_top_k,
        "show_scores": show_scores,
    }

    qa_results = []
    progress_bar = (
        tqdm(total=len(queries), desc="Evaluating LongMemEval QA")
        if show_progress and sys.stderr.isatty()
        else None
    )
    for index, query in enumerate(queries, start=1):
        qid = query["id"]
        search_result = results_by_id.get(qid, {"results": []})
        retrieved = search_result.get("results", [])[:qa_top_k]

        if (
            qid in existing
            and _is_complete_qa_result(existing[qid], answer_prompt_version, memory_format, expected_config)
        ):
            existing_item = dict(existing[qid])
            _attach_retrieval_diagnostics(existing_item, query, retrieved)
            qa_results.append(existing_item)
            _update_qa_progress(
                progress_bar,
                qa_results,
                total=len(queries),
                current=index,
                question_id=qid,
                started=started,
                reused=True,
                enabled=show_progress,
            )
            continue

        question = query["text"]
        question_type = (query.get("metadata") or {}).get("question_type", "unknown")
        question_date = (query.get("metadata") or {}).get("question_date", "")
        correct_answer = (query.get("task") or {}).get("correct_answer", "")

        answer_prompt = format_answer_prompt(
            question=question,
            question_date=question_date,
            retrieved=retrieved,
            question_type=question_type,
            answer_prompt_version=answer_prompt_version,
            memory_format=memory_format,
            show_scores=show_scores,
        )
        answer = answerer_client.chat(
            model=answerer_model,
            messages=[
                {"role": "system", "content": ANSWER_SYSTEM},
                {"role": "user", "content": answer_prompt},
            ],
            temperature=0.0,
            max_tokens=1024,
        )

        judge_prompt = get_judge_prompt(
            question_type=question_type,
            question_id=qid,
            question=question,
            answer=str(correct_answer),
            response=answer.content,
        )
        judgment = judge_client.chat(
            model=judge_model,
            messages=[{"role": "user", "content": judge_prompt}],
            temperature=0.0,
            max_tokens=256,
        )
        correct = parse_yes_no(judgment.content)

        result_item = {
            "question_id": qid,
            "question": question,
            "question_type": question_type,
            "is_abstention": qid.endswith("_abs"),
            "correct_answer": correct_answer,
            "generated_answer": answer.content,
            "judge_response": judgment.content,
            "correct": correct,
            "qa_top_k": qa_top_k,
            "answer_prompt_version": answer_prompt_version,
            "memory_format": memory_format,
            "show_scores": show_scores,
            "retrieved_count": len(retrieved),
            "answerer": {
                "model": answerer_model,
                "latency_ms": round(answer.latency_ms, 1),
                "prompt_tokens": answer.prompt_tokens,
                "completion_tokens": answer.completion_tokens,
                "total_tokens": answer.total_tokens,
            },
            "judge": {
                "model": judge_model,
                "latency_ms": round(judgment.latency_ms, 1),
                "prompt_tokens": judgment.prompt_tokens,
                "completion_tokens": judgment.completion_tokens,
                "total_tokens": judgment.total_tokens,
            },
        }
        _attach_retrieval_diagnostics(result_item, query, retrieved)
        qa_results.append(result_item)
        _write_json(output_results_path, qa_results)
        _update_qa_progress(
            progress_bar,
            qa_results,
            total=len(queries),
            current=index,
            question_id=qid,
            started=started,
            reused=False,
            enabled=show_progress,
        )

    if progress_bar is not None:
        progress_bar.close()

    metrics = compute_qa_metrics(
        qa_results,
        answer_prompt_version=answer_prompt_version,
        memory_format=memory_format,
    )
    _write_json(output_results_path, qa_results)
    _write_json(output_metrics_path, metrics)
    return metrics


def compute_qa_metrics(
    results: list[dict],
    answer_prompt_version: str | None = None,
    memory_format: str | None = None,
) -> dict:
    attach_error_analysis(results)
    by_type: dict[str, list[dict]] = defaultdict(list)
    for item in results:
        by_type[item.get("question_type", "unknown")].append(item)

    prompt_version = answer_prompt_version or _infer_answer_prompt_version(results)
    memory_format_value = memory_format or _infer_memory_format(results)
    return {
        "num_questions": len(results),
        "answer_prompt_version": prompt_version,
        "memory_format": memory_format_value,
        "overall": _summarize(results),
        "by_type": {qtype: _summarize(items) for qtype, items in sorted(by_type.items())},
        "diagnostics": _summarize_retrieval_diagnostics(results),
        "error_analysis": _summarize_error_analysis(results),
    }


def parse_yes_no(text: str) -> bool:
    normalized = text.strip().lower()
    matches = list(re.finditer(r"\b(yes|no)\b", normalized))
    if matches:
        return matches[-1].group(1) == "yes"
    return "yes" in normalized and "no" not in normalized


def _attach_retrieval_diagnostics(
    result_item: dict,
    query: dict,
    retrieved: list[dict],
) -> None:
    """Add evidence-hit diagnostics without changing QA correctness."""
    gold_turn_ids = _gold_turn_ids(query)
    gold_turn_id_set = set(gold_turn_ids)
    top_result_ids = [str(item.get("id", "")) for item in retrieved if item.get("id")]
    ranks = [
        index + 1
        for index, item in enumerate(retrieved)
        if str(item.get("id", "")) in gold_turn_id_set
    ]
    has_gold = bool(gold_turn_ids)
    gold_hit = bool(ranks) if has_gold else None

    result_item["retrieval_diagnostic"] = {
        "gold_turn_ids": gold_turn_ids,
        "top_result_ids": top_result_ids,
        "gold_turn_retrieved": gold_hit,
        "gold_turn_ranks": ranks,
        "gold_turn_best_rank": min(ranks) if ranks else None,
        "diagnosis": _diagnosis_label(result_item.get("correct"), has_gold, gold_hit),
    }


def _gold_turn_ids(query: dict) -> list[str]:
    ids = (query.get("task") or {}).get("gold_turn_ids") or []
    return [str(item) for item in ids if item]


def _diagnosis_label(correct: bool | None, has_gold: bool, gold_hit: bool | None) -> str:
    if not has_gold:
        return "no_gold_reference"
    if correct is True and gold_hit is True:
        return "correct_with_gold_hit"
    if correct is True and gold_hit is False:
        return "correct_without_gold_hit"
    if correct is False and gold_hit is True:
        return "wrong_despite_gold_hit"
    if correct is False and gold_hit is False:
        return "wrong_with_gold_miss"
    return "unknown"


def _summarize_retrieval_diagnostics(results: list[dict]) -> dict:
    counters = {
        "total": len(results),
        "with_gold_reference": 0,
        "without_gold_reference": 0,
        "gold_hit": 0,
        "gold_miss": 0,
        "correct_gold_hit": 0,
        "correct_gold_miss": 0,
        "wrong_gold_hit": 0,
        "wrong_gold_miss": 0,
        "no_gold_correct": 0,
        "no_gold_wrong": 0,
    }

    for item in results:
        diagnostic = item.get("retrieval_diagnostic") or {}
        gold_hit = diagnostic.get("gold_turn_retrieved")
        correct = item.get("correct")

        if gold_hit is None:
            counters["without_gold_reference"] += 1
            if correct is True:
                counters["no_gold_correct"] += 1
            elif correct is False:
                counters["no_gold_wrong"] += 1
            continue

        counters["with_gold_reference"] += 1
        if gold_hit is True:
            counters["gold_hit"] += 1
            if correct is True:
                counters["correct_gold_hit"] += 1
            elif correct is False:
                counters["wrong_gold_hit"] += 1
        else:
            counters["gold_miss"] += 1
            if correct is True:
                counters["correct_gold_miss"] += 1
            elif correct is False:
                counters["wrong_gold_miss"] += 1

    with_gold = counters["with_gold_reference"]
    counters["gold_hit_rate"] = counters["gold_hit"] / with_gold if with_gold else 0.0
    return counters


def load_and_evaluate_qa(
    search_results_path: str,
    prepared_path: str,
    output_results_path: str,
    output_metrics_path: str,
    answerer_model: str,
    judge_model: str,
    llm_api_key_env: str = "OPENROUTER_API_KEY",
    llm_base_url: str = "https://openrouter.ai/api/v1",
    llm_thinking: str | None = None,
    qa_top_k: int = 10,
    resume: bool = False,
    answer_prompt_version: str = ANSWER_PROMPT_VERSION_DEFAULT,
    memory_format: str = MEMORY_FORMAT_DEFAULT,
    show_scores: bool = False,
) -> dict:
    _validate_answer_prompt_version(answer_prompt_version)
    _validate_memory_format(memory_format)
    with open(search_results_path, "r", encoding="utf-8") as f:
        search_results = json.load(f)
    with open(prepared_path, "r", encoding="utf-8") as f:
        prepared = json.load(f)

    expected_config = {
        "answerer_model": answerer_model,
        "judge_model": judge_model,
        "qa_top_k": qa_top_k,
        "show_scores": show_scores,
    }

    if resume and _has_complete_existing_results(
        output_results_path, prepared, answer_prompt_version, memory_format,
        expected_config=expected_config,
    ):
        client = _ExistingResultsOnlyClient()
    else:
        client = OpenAICompatibleClient(
            api_key_env=llm_api_key_env,
            base_url=llm_base_url,
            thinking=llm_thinking,
        )

    return evaluate_qa(
        search_results=search_results,
        prepared=prepared,
        output_results_path=output_results_path,
        output_metrics_path=output_metrics_path,
        answerer_client=client,
        judge_client=client,
        answerer_model=answerer_model,
        judge_model=judge_model,
        qa_top_k=qa_top_k,
        resume=resume,
        answer_prompt_version=answer_prompt_version,
        memory_format=memory_format,
        show_progress=True,
        show_scores=show_scores,
    )


def _summarize(items: list[dict]) -> dict:
    if not items:
        return {
            "accuracy": 0.0,
            "correct": 0,
            "total": 0,
            "avg_answer_latency_ms": 0.0,
            "avg_judge_latency_ms": 0.0,
            "avg_total_latency_ms": 0.0,
            "avg_total_tokens": 0.0,
            "avg_context_tokens": 0.0,
        }

    correct = sum(1 for item in items if item.get("correct") is True)
    answer_latency = [_nested_number(item, "answerer", "latency_ms") for item in items]
    judge_latency = [_nested_number(item, "judge", "latency_ms") for item in items]
    answer_tokens = [_nested_number(item, "answerer", "total_tokens") for item in items]
    judge_tokens = [_nested_number(item, "judge", "total_tokens") for item in items]
    context_tokens = [_nested_number(item, "answerer", "prompt_tokens") for item in items]
    return {
        "accuracy": correct / len(items),
        "correct": correct,
        "total": len(items),
        "avg_answer_latency_ms": _avg(answer_latency),
        "avg_judge_latency_ms": _avg(judge_latency),
        "avg_total_latency_ms": _avg(_sum_pairs(answer_latency, judge_latency)),
        "avg_total_tokens": _avg(_sum_pairs(answer_tokens, judge_tokens)),
        "avg_context_tokens": _avg(context_tokens),
    }


def _avg(values: list[float]) -> float:
    filtered = [float(value) for value in values if value is not None]
    return sum(filtered) / len(filtered) if filtered else 0.0


def _nested_number(item: dict, section: str, field: str) -> float | None:
    value = (item.get(section) or {}).get(field)
    if isinstance(value, (int, float)):
        return float(value)
    return None


def _sum_pairs(left: list[float | None], right: list[float | None]) -> list[float]:
    return [
        float(a) + float(b)
        for a, b in zip(left, right)
        if a is not None and b is not None
    ]


def _update_qa_progress(
    progress_bar: Any | None,
    results: list[dict],
    total: int,
    current: int,
    question_id: str,
    started: float,
    reused: bool,
    enabled: bool,
) -> None:
    if not enabled:
        return
    correct = sum(1 for item in results if item.get("correct") is True)
    wrong = sum(1 for item in results if item.get("correct") is False)
    complete = correct + wrong
    accuracy = correct / complete if complete else 0.0
    if progress_bar is not None:
        progress_bar.update(1)
        progress_bar.set_postfix(
            correct=correct,
            wrong=wrong,
            acc=f"{accuracy:.1%}",
            last=question_id,
            refresh=True,
        )
        return

    interval = _progress_interval(total)
    if current != 1 and current != total and current % interval != 0:
        return
    elapsed_s = time.monotonic() - started
    label = "resume" if reused else "done"
    print(
        f"[qa] {current}/{total} {label} | correct={correct} wrong={wrong} "
        f"| acc={accuracy:.1%} | last={question_id} | elapsed={elapsed_s:.1f}s",
        flush=True,
    )


def _progress_interval(total: int) -> int:
    if total <= 100:
        return 10
    if total <= 1000:
        return 50
    return 100


def _load_existing_results(path: str) -> dict[str, dict]:
    if not os.path.isfile(path):
        return {}
    with open(path, "r", encoding="utf-8") as f:
        return {item["question_id"]: item for item in json.load(f)}


def _has_complete_existing_results(
    path: str,
    prepared: dict,
    answer_prompt_version: str,
    memory_format: str,
    expected_config: dict | None = None,
) -> bool:
    existing = _load_existing_results(path)
    if not existing:
        return False
    query_ids = [query.get("id") for query in prepared.get("queries", [])]
    return bool(query_ids) and all(
        query_id in existing
        and _is_complete_qa_result(
            existing[query_id], answer_prompt_version, memory_format, expected_config
        )
        for query_id in query_ids
    )


def _is_complete_qa_result(
    item: dict,
    answer_prompt_version: str,
    memory_format: str,
    expected_config: dict | None = None,
) -> bool:
    base_check = (
        _matches_answer_prompt_version(item, answer_prompt_version)
        and _matches_memory_format(item, memory_format)
        and isinstance(item.get("correct"), bool)
        and bool(str(item.get("generated_answer") or "").strip())
        and bool(str(item.get("judge_response") or "").strip())
    )
    if not base_check or expected_config is None:
        return base_check
    stored_model = (item.get("answerer") or {}).get("model", "")
    if stored_model and stored_model != expected_config.get("answerer_model"):
        return False
    stored_judge_model = (item.get("judge") or {}).get("model", "")
    if stored_judge_model and stored_judge_model != expected_config.get("judge_model"):
        return False
    stored_topk = item.get("qa_top_k")
    if stored_topk is not None and stored_topk != expected_config.get("qa_top_k"):
        return False
    stored_show_scores = item.get("show_scores")
    if stored_show_scores is not None and stored_show_scores != expected_config.get("show_scores"):
        return False
    return True


def _matches_answer_prompt_version(item: dict, answer_prompt_version: str) -> bool:
    stored = item.get("answer_prompt_version") or ANSWER_PROMPT_VERSION_DEFAULT
    return stored == answer_prompt_version


def _matches_memory_format(item: dict, memory_format: str) -> bool:
    stored = item.get("memory_format") or MEMORY_FORMAT_DEFAULT
    return stored == memory_format


def _infer_answer_prompt_version(results: list[dict]) -> str:
    versions = {
        item.get("answer_prompt_version") or ANSWER_PROMPT_VERSION_DEFAULT
        for item in results
    }
    if not versions:
        return ANSWER_PROMPT_VERSION_DEFAULT
    if len(versions) == 1:
        return next(iter(versions))
    return "mixed"


def _infer_memory_format(results: list[dict]) -> str:
    formats = {
        item.get("memory_format") or MEMORY_FORMAT_DEFAULT
        for item in results
    }
    if not formats:
        return MEMORY_FORMAT_DEFAULT
    if len(formats) == 1:
        return next(iter(formats))
    return "mixed"


class _ExistingResultsOnlyClient:
    def chat(self, *args: object, **kwargs: object) -> None:
        raise IncompleteResultsError(
            "Existing QA results are incomplete. Provide a valid LLM API key to continue "
            "generating missing answers, or delete the partial QA result file and rerun."
        )


def _write_json(path: str, data: object) -> None:
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f, ensure_ascii=False, indent=2)


def main() -> None:
    parser = argparse.ArgumentParser(description="Evaluate LongMemEval QA accuracy")
    parser.add_argument("--search-results", required=True)
    parser.add_argument("--prepared", required=True)
    parser.add_argument("--output-results", required=True)
    parser.add_argument("--output-metrics", required=True)
    parser.add_argument("--answerer-model", required=True)
    parser.add_argument("--judge-model", required=True)
    parser.add_argument("--llm-api-key-env", default="OPENROUTER_API_KEY")
    parser.add_argument("--llm-base-url", default="https://openrouter.ai/api/v1")
    parser.add_argument(
        "--llm-thinking",
        choices=["default", "enabled", "disabled"],
        default="default",
    )
    parser.add_argument("--qa-top-k", type=int, default=10)
    parser.add_argument("--resume", action="store_true")
    parser.add_argument(
        "--answer-prompt-version",
        choices=ANSWER_PROMPT_VERSIONS,
        default=ANSWER_PROMPT_VERSION_DEFAULT,
    )
    parser.add_argument(
        "--memory-format",
        choices=MEMORY_FORMATS,
        default=MEMORY_FORMAT_DEFAULT,
    )
    args = parser.parse_args()

    metrics = load_and_evaluate_qa(
        search_results_path=args.search_results,
        prepared_path=args.prepared,
        output_results_path=args.output_results,
        output_metrics_path=args.output_metrics,
        answerer_model=args.answerer_model,
        judge_model=args.judge_model,
        llm_api_key_env=args.llm_api_key_env,
        llm_base_url=args.llm_base_url,
        llm_thinking=None if args.llm_thinking == "default" else args.llm_thinking,
        qa_top_k=args.qa_top_k,
        resume=args.resume,
        answer_prompt_version=args.answer_prompt_version,
        memory_format=args.memory_format,
    )
    print(json.dumps(metrics, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
