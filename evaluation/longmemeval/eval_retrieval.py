"""Evaluate retrieval results against LongMemEval ground truth.

Computes session-level and turn-level retrieval metrics (recall@K, MRR,
nDCG@K) grouped overall and by question type.

Expects search results in the prepared-schema QueryOutput format:
  [
    {
      "query_id": "q001",
      "query": "...",
      "filter": {"scope_id": "lme_user_0"},
      "metadata": {"question_type": "single-session-user"},
      "task": {...},
      "results": [
        {"id": "...", "text": "...", "metadata": {"scope_id": "...", "session_id": "...", "has_answer": true}, "score": 0.9}
      ]
    }
  ]
"""

import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from common.metrics import mrr, ndcg_at_k, recall_at_k
from longmemeval.provenance import (
    build_source_turn_metadata,
    retrieved_source_session_ids,
    retrieved_source_turn_ids,
)

# Default K values for recall and nDCG
_DEFAULT_RECALL_KS = [1, 3, 5, 10]
_DEFAULT_NDCG_KS = [5, 10]


def _avg(metrics: dict[str, list[float]]) -> dict[str, float]:
    """Average each list of floats in *metrics*."""
    return {key: sum(vals) / len(vals) if vals else 0.0 for key, vals in metrics.items()}


def _collect_metrics(
    retrieved_ids: list[str],
    relevant_ids: list[str],
    ks: list[int],
    ndcg_ks: list[int],
) -> dict[str, float]:
    """Compute all metrics for a single query."""
    result: dict[str, float] = {}
    for k in ks:
        result[f"recall@{k}"] = recall_at_k(retrieved_ids, relevant_ids, k)
    result["mrr"] = mrr(retrieved_ids, relevant_ids)
    for k in ndcg_ks:
        result[f"ndcg@{k}"] = ndcg_at_k(retrieved_ids, relevant_ids, k)
    return result


def _build_results_by_id(search_results: list[dict]) -> dict[str, dict]:
    """Index search results by query_id."""
    return {r["query_id"]: r for r in search_results}


def _extract_answer_turn_ids_from_result(result: dict) -> list[str]:
    """Fallback: return retrieved IDs whose metadata has has_answer=True."""
    return [
        item["id"]
        for item in result.get("results", [])
        if (item.get("metadata") or {}).get("has_answer") is True
    ]


def _extract_gold_turn_ids_from_lme(item: dict) -> list[str]:
    """Rebuild preprocessor turn IDs for all LongMemEval has_answer turns."""
    gold_ids: list[str] = []
    question_id = item["question_id"]
    for session_idx, session in enumerate(item.get("haystack_sessions", [])):
        for turn_idx, turn in enumerate(session):
            if turn.get("has_answer") is True:
                gold_ids.append(f"{question_id}_s{session_idx}_t{turn_idx}")
    return gold_ids


def _gold_session_ids(search_result: dict | None, lme_item: dict) -> list[str]:
    task = (search_result or {}).get("task") or {}
    ids = task.get("gold_session_ids")
    if isinstance(ids, list):
        return [str(item) for item in ids]
    return [str(item) for item in lme_item.get("answer_session_ids", [])]


def _gold_turn_ids(search_result: dict | None, lme_item: dict) -> list[str]:
    task = (search_result or {}).get("task") or {}
    ids = task.get("gold_turn_ids")
    if isinstance(ids, list):
        return [str(item) for item in ids]
    rebuilt = _extract_gold_turn_ids_from_lme(lme_item)
    if rebuilt:
        return rebuilt
    if search_result is not None:
        return _extract_answer_turn_ids_from_result(search_result)
    return []


def evaluate_retrieval(
    search_results: list[dict],
    lme_data: list[dict],
    source_turn_metadata: dict[str, dict],
    ks: list[int] | None = None,
    expected_query_ids: list[str] | None = None,
) -> dict:
    """Evaluate retrieval quality against LongMemEval ground truth.

    Parameters
    ----------
    search_results : list[dict]
        Each element has ``query_id`` and ``results`` (list of retrieved
        items with ``id``, ``metadata`` containing ``session_id`` and
        ``has_answer``, and ``score``).  May also have ``metadata`` with
        ``question_type``.
    lme_data : list[dict]
        Each element has ``question_id``, ``question_type``, and
        ``answer_session_ids``.
    source_turn_metadata : dict[str, dict]
        Raw prepared-memory metadata keyed by source turn ID. Session scoring
        never trusts metadata copied onto retrieved or extracted records.
    ks : list[int] | None
        Recall@K cutoffs.  Defaults to [1, 3, 5, 10].

    Returns
    -------
    dict
        Nested structure with ``session`` and ``turn`` keys, each containing
        ``overall`` and ``by_type`` metrics.
    """
    if ks is None:
        ks = _DEFAULT_RECALL_KS
    ndcg_ks = _DEFAULT_NDCG_KS

    results_by_id = _build_results_by_id(search_results)
    lme_by_id = {item["question_id"]: item for item in lme_data}

    query_ids = expected_query_ids if expected_query_ids is not None else list(lme_by_id)

    # Exclude abstention questions (question_id ending with "_abs")
    non_abstention_ids = [qid for qid in query_ids if not qid.endswith("_abs")]

    # Accumulators keyed by metric name -> list of per-query values.
    # Two levels: session and turn, each split by overall / per-type.
    session_overall: dict[str, list[float]] = {}
    turn_overall: dict[str, list[float]] = {}
    session_by_type: dict[str, dict[str, list[float]]] = {}
    turn_by_type: dict[str, dict[str, list[float]]] = {}

    missing_results = 0

    for qid in non_abstention_ids:
        if qid not in lme_by_id:
            missing_results += 1
            continue
        lme_item = lme_by_id[qid]
        sr = results_by_id.get(qid)
        if sr is None:
            missing_results += 1
            sr = {"query_id": qid, "results": []}

        # Prefer question_type from search result metadata; fall back to lme_data
        qtype = (
            (sr.get("metadata") or {}).get("question_type")
            or lme_item.get("question_type", "unknown")
        )

        # --- Session-level ---
        retrieved_sessions = retrieved_source_session_ids(sr, source_turn_metadata)
        answer_sessions = _gold_session_ids(sr, lme_item)
        s_metrics = _collect_metrics(retrieved_sessions, answer_sessions, ks, ndcg_ks)

        for metric_name, value in s_metrics.items():
            session_overall.setdefault(metric_name, []).append(value)
            session_by_type.setdefault(qtype, {}).setdefault(metric_name, []).append(value)

        # --- Turn-level ---
        retrieved_turn_ids = retrieved_source_turn_ids(sr)
        relevant_turn_ids = _gold_turn_ids(sr, lme_item)
        t_metrics = _collect_metrics(retrieved_turn_ids, relevant_turn_ids, ks, ndcg_ks)

        for metric_name, value in t_metrics.items():
            turn_overall.setdefault(metric_name, []).append(value)
            turn_by_type.setdefault(qtype, {}).setdefault(metric_name, []).append(value)

    return {
        "num_questions": len(non_abstention_ids),
        "num_evaluated": len(non_abstention_ids),
        "num_missing_results": missing_results,
        "num_abstention_excluded": len(query_ids) - len(non_abstention_ids),
        "session": {
            "overall": _avg(session_overall),
            "by_type": {qt: _avg(metrics) for qt, metrics in session_by_type.items()},
        },
        "turn": {
            "overall": _avg(turn_overall),
            "by_type": {qt: _avg(metrics) for qt, metrics in turn_by_type.items()},
        },
    }


def load_and_evaluate(
    search_results_path: str,
    lme_data_path: str,
    output_path: str,
    prepared_path: str | None = None,
) -> dict:
    """Load JSON files, run evaluation, and write results to *output_path*."""
    with open(search_results_path, "r", encoding="utf-8") as f:
        search_results = json.load(f)
    with open(lme_data_path, "r", encoding="utf-8") as f:
        lme_data = json.load(f)
    if prepared_path is None:
        raise ValueError("prepared_path is required for raw source-turn metadata")
    with open(prepared_path, "r", encoding="utf-8") as f:
        prepared = json.load(f)
    expected_query_ids = [
        query["id"]
        for query in prepared.get("queries", [])
        if isinstance(query, dict) and "id" in query
    ]
    source_turn_metadata = build_source_turn_metadata(prepared)

    report = evaluate_retrieval(
        search_results,
        lme_data,
        source_turn_metadata,
        expected_query_ids=expected_query_ids,
    )

    os.makedirs(os.path.dirname(output_path) or ".", exist_ok=True)
    with open(output_path, "w", encoding="utf-8") as f:
        json.dump(report, f, indent=2, ensure_ascii=False)

    return report
