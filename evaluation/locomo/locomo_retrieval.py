import argparse
import json
import math
import re
import sys
from collections import defaultdict
from pathlib import Path

EVALUATION_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(EVALUATION_ROOT))

from common.report import (
    fmt_float,
    fmt_int,
    fmt_percent,
    generate_report,
    html_escape,
    render_card,
    render_metric_value,
    render_text_cell,
)
from locomo.locomo_provenance import query_ref, result_evidence_ids

RESULT_PATH_RE = re.compile(r"^\$\[(\d+)\]\.conversation\.session_(\d+)\[(\d+)\]\.text$")
QUERY_PATH_RE = re.compile(r"^\$\[(\d+)\]\.qa\[(\d+)\]\.question$")


def main():
    parser = argparse.ArgumentParser(description="Evaluate LoCoMo retrieval evidence hit diagnostics.")
    parser.add_argument("--dataset", type=Path, required=True)
    parser.add_argument("--input", type=Path, required=True, help="search results JSON")
    parser.add_argument(
        "--input-format",
        choices=("memory-bench", "mem0", "prepared-raw", "prepared-extracted"),
        default="memory-bench",
    )
    parser.add_argument("--output-json", type=Path, required=True)
    parser.add_argument("--html-report", type=Path, required=True)
    args = parser.parse_args()

    dataset = json.loads(args.dataset.read_text(encoding="utf-8"))
    search_results = json.loads(args.input.read_text(encoding="utf-8"))
    if args.input_format == "mem0":
        report = unsupported_mem0_report(args.input)
    elif args.input_format.startswith("prepared-"):
        report = evaluate_retrieval(
            dataset,
            search_results,
            args.input,
            prepared_mode=args.input_format.removeprefix("prepared-"),
        )
    else:
        report = evaluate_retrieval(dataset, search_results, args.input)
    args.output_json.parent.mkdir(parents=True, exist_ok=True)
    args.output_json.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    write_html_report(args.html_report, report)
    print(f"LoCoMo retrieval metrics saved to {args.output_json}")
    print(f"LoCoMo retrieval HTML report saved to {args.html_report}")


def evaluate_retrieval(
    dataset,
    search_results,
    input_path,
    prepared_mode=None,
):
    rows = []
    by_category = defaultdict(list)
    for item in search_results:
        row = (
            evaluate_prepared_query(dataset, item, prepared_mode)
            if prepared_mode is not None
            else evaluate_query(dataset, item)
        )
        if row is None:
            continue
        rows.append(row)

    scored = [row for row in rows if row["evidence_count"] > 0]
    for row in scored:
        by_category[str(row["category"])].append(row)
    overall = summarize_rows(scored)
    by_category_summary = {
        category: summarize_rows(items)
        for category, items in sorted(by_category.items(), key=lambda value: int(value[0]))
    }
    failures = [row for row in scored if row["evidence_hit"] < 1.0]
    failures.sort(key=lambda row: (row["evidence_hit"], row["first_hit_rank"] or 9999))
    return {
        "dataset": "locomo",
        "input": str(input_path),
        "overall": overall,
        "by_category": by_category_summary,
        "per_query": rows,
        "failures": failures[:100],
    }


def evaluate_prepared_query(dataset, item, mode):
    ref = query_ref(item)
    try:
        question = dataset[ref.sample_index]["qa"][ref.question_index]
    except (IndexError, KeyError, TypeError) as error:
        raise ValueError(f"invalid prepared query reference: {ref}") from error

    gold_ids = {
        normalize_evidence_id(value, ref.sample_index)
        for value in question.get("evidence", [])
    }
    evidence_by_rank = [
        set(result_evidence_ids(result, mode))
        for result in item.get("results", [])
    ]
    evidence_ref_counts = [
        _result_evidence_ref_count(result, mode)
        for result in item.get("results", [])
    ]
    covered = set()
    first_hit_rank = None
    for rank, evidence_ids in enumerate(evidence_by_rank, start=1):
        covered.update(evidence_ids & gold_ids)
        if first_hit_rank is None and evidence_ids & gold_ids:
            first_hit_rank = rank

    unique_expanded = set().union(*evidence_by_rank) if evidence_by_rank else set()
    evidence_count = len(gold_ids)
    retrieved_count = len(evidence_by_rank)
    evidence_ref_count = sum(evidence_ref_counts)
    return {
        "query_id": item.get("query_id"),
        "query_path": item.get("query_path"),
        "question": question.get("question", ""),
        "answer": question.get("answer", ""),
        "category": int(question.get("category", -1)),
        "evidence": sorted(gold_ids),
        "retrieved_evidence": [sorted(evidence_ids) for evidence_ids in evidence_by_rank],
        "evidence_count": evidence_count,
        "evidence_hit": len(covered) / evidence_count if evidence_count else 0.0,
        "first_hit_rank": first_hit_rank,
        "mrr": 1.0 / first_hit_rank if first_hit_rank else 0.0,
        "retrieved_count": retrieved_count,
        "retrieved_evidence_ref_count": evidence_ref_count,
        "expanded_source_turn_count": len(unique_expanded),
        "evidence_refs_per_result": (
            evidence_ref_count / retrieved_count if retrieved_count else 0.0
        ),
        "context_tokens": sum(
            estimate_tokens(result.get("text", ""))
            for result in item.get("results", [])
        ),
    }


def _result_evidence_ref_count(result, mode):
    if mode == "raw":
        return 1 if result.get("id") else 0
    refs = (result.get("metadata") or {}).get("evidence_refs") or []
    return len(
        {
            (
                str(ref.get("message_id") or ""),
                int(ref.get("start_char") or 0),
                int(ref.get("end_char") or 0),
                str(ref.get("evidence_role") or ""),
            )
            for ref in refs
            if ref.get("message_id")
        }
    )


def unsupported_mem0_report(input_path):
    reason = (
        "LoCoMo mem0 search output contains extracted memory text but not original "
        "conversation turn paths, so D{session}:{turn} evidence_hit@k cannot be "
        "computed with the same rule as memory-bench."
    )
    return {
        "dataset": "locomo",
        "input": str(input_path),
        "supported": False,
        "unsupported_reason": reason,
        "overall": {
            "count": 0,
            "evidence_hit_at_k": None,
            "evidence_mrr": None,
            "avg_retrieved_contexts": None,
            "avg_context_tokens": None,
            "missing_evidence_count": None,
        },
        "by_category": {},
        "failures": [],
    }


def evaluate_query(dataset, item):
    query_match = QUERY_PATH_RE.match(str(item.get("query_path", "")))
    if not query_match:
        return None
    sample_index, question_index = (int(value) for value in query_match.groups())
    try:
        question = dataset[sample_index]["qa"][question_index]
    except (IndexError, KeyError, TypeError):
        return None

    gold_ids = {normalize_evidence_id(value, sample_index) for value in question.get("evidence", [])}
    retrieved_ids = []
    retrieved_text_tokens = 0
    for result in item.get("results", []):
        path = (result.get("metadata") or {}).get("path", "")
        evidence_id = evidence_id_from_result_path(path)
        if evidence_id:
            retrieved_ids.append(evidence_id)
        retrieved_text_tokens += estimate_tokens(result.get("text", ""))

    first_hit_rank = None
    hits = 0
    for rank, evidence_id in enumerate(retrieved_ids, start=1):
        if evidence_id in gold_ids:
            hits += 1
            if first_hit_rank is None:
                first_hit_rank = rank
    evidence_count = len(gold_ids)
    evidence_hit = hits / evidence_count if evidence_count else 0.0
    return {
        "query_path": item.get("query_path"),
        "question": question.get("question", ""),
        "answer": question.get("answer", ""),
        "category": int(question.get("category", -1)),
        "evidence": sorted(gold_ids),
        "retrieved_evidence": retrieved_ids,
        "evidence_count": evidence_count,
        "evidence_hit": evidence_hit,
        "first_hit_rank": first_hit_rank,
        "mrr": 1.0 / first_hit_rank if first_hit_rank else 0.0,
        "retrieved_count": len(retrieved_ids),
        "context_tokens": retrieved_text_tokens,
    }


def normalize_evidence_id(value, sample_index):
    match = re.match(r"^D(\d+):(\d+)$", str(value).strip())
    if not match:
        return str(value).strip()
    session, turn = (int(part) for part in match.groups())
    return f"S{sample_index}:D{session}:{turn}"


def evidence_id_from_result_path(path):
    match = RESULT_PATH_RE.match(str(path))
    if not match:
        return None
    sample_index, session_number, message_index = (int(value) for value in match.groups())
    return f"S{sample_index}:D{session_number}:{message_index}"


def summarize_rows(rows):
    if not rows:
        return {
            "count": 0,
            "evidence_hit_at_k": 0.0,
            "evidence_mrr": 0.0,
            "avg_retrieved_contexts": 0.0,
            "avg_context_tokens": 0.0,
            "missing_evidence_count": 0,
            "avg_evidence_refs_per_result": 0.0,
            "avg_expanded_source_turns": 0.0,
        }
    return {
        "count": len(rows),
        "evidence_hit_at_k": mean(row["evidence_hit"] for row in rows),
        "evidence_mrr": mean(row["mrr"] for row in rows),
        "avg_retrieved_contexts": mean(row["retrieved_count"] for row in rows),
        "avg_context_tokens": mean(row["context_tokens"] for row in rows),
        "missing_evidence_count": sum(1 for row in rows if row["evidence_hit"] < 1.0),
        "avg_evidence_refs_per_result": mean(
            row.get("evidence_refs_per_result", 1.0) for row in rows
        ),
        "avg_expanded_source_turns": mean(
            row.get("expanded_source_turn_count", row["retrieved_count"])
            for row in rows
        ),
    }


def write_html_report(path, report):
    overall = report.get("overall", {})
    scorecard = "".join(
        [
            render_card("Evidence Hit@K", fmt_percent(overall.get("evidence_hit_at_k")), "gold evidence coverage", score_value=overall.get("evidence_hit_at_k")),
            render_card("Evidence MRR", fmt_float(overall.get("evidence_mrr"), 4), "first evidence rank", score_value=overall.get("evidence_mrr")),
            render_card("Retrieved Tokens", fmt_int(overall.get("avg_context_tokens")), "avg estimated tokens"),
            render_card("Missing Evidence", fmt_int(overall.get("missing_evidence_count")), "queries with incomplete evidence"),
        ]
    )
    sections = [
        {"title": "Retrieval Summary", "html": render_summary_table(overall)},
        {"title": "Retrieval by Category", "html": render_category_table(report.get("by_category", {}))},
        {
            "title": "Missing Evidence Examples",
            "subtitle": "Queries with incomplete gold evidence coverage are shown first.",
            "html": render_failure_table(report.get("failures", [])),
        },
    ]
    warnings = []
    if not report.get("supported", True):
        warnings.append(report.get("unsupported_reason", "Retrieval evidence diagnostics are unsupported."))
    generate_report(
        output_path=str(path),
        title="RAM-A LoCoMo Retrieval Report",
        header_meta={"Dataset": "LoCoMo", "Input": report.get("input", "unknown")},
        scorecard_html=scorecard,
        sections=sections,
        warnings=warnings,
        run_meta={"input": report.get("input")},
    )


def render_summary_table(overall):
    rows = [
        ("Evidence Hit@K", render_metric_value(overall.get("evidence_hit_at_k"))),
        ("Evidence MRR", render_metric_value(overall.get("evidence_mrr"))),
        ("Avg Retrieved Contexts", fmt_float(overall.get("avg_retrieved_contexts"), 2)),
        ("Retrieved Context Tokens", fmt_float(overall.get("avg_context_tokens"), 2)),
        ("Evidence Refs / Result", fmt_float(overall.get("avg_evidence_refs_per_result"), 2)),
        ("Expanded Source Turns", fmt_float(overall.get("avg_expanded_source_turns"), 2)),
        ("Missing Evidence Count", fmt_int(overall.get("missing_evidence_count"))),
        ("Count", fmt_int(overall.get("count"))),
    ]
    body = "".join(
        f'<tr><td>{html_escape(name)}</td><td class="mono">{value}</td></tr>'
        for name, value in rows
    )
    return (
        '<div class="table-wrap"><table><thead><tr><th>Metric</th><th class="mono">Value</th></tr></thead>'
        f"<tbody>{body}</tbody></table></div>"
    )


def render_category_table(by_category):
    if not by_category:
        return '<p class="subtle">No category rows available.</p>'
    rows = []
    for category, scores in sorted(by_category.items(), key=lambda item: int(item[0])):
        rows.append(
            "<tr>"
            f'<td class="mono">{html_escape(category)}</td>'
            f'<td class="mono">{render_metric_value(scores.get("evidence_hit_at_k"))}</td>'
            f'<td class="mono">{render_metric_value(scores.get("evidence_mrr"))}</td>'
            f'<td class="mono">{fmt_float(scores.get("avg_context_tokens"), 2)}</td>'
            f'<td class="mono">{fmt_int(scores.get("missing_evidence_count"))}</td>'
            f'<td class="mono">{fmt_int(scores.get("count"))}</td>'
            "</tr>"
        )
    return (
        '<div class="table-wrap"><table><thead><tr>'
        '<th class="mono">Category</th><th class="mono">Hit@K</th><th class="mono">MRR</th>'
        '<th class="mono">Retrieved Tokens</th><th class="mono">Missing</th><th class="mono">Count</th>'
        f"</tr></thead><tbody>{''.join(rows)}</tbody></table></div>"
    )


def render_failure_table(items):
    if not items:
        return '<p class="subtle">No missing evidence rows available.</p>'
    rows = []
    for item in items:
        rows.append(
            "<tr>"
            f'<td class="mono">{html_escape(item.get("category"))}</td>'
            f'{render_text_cell(item.get("question", ""))}'
            f'{render_text_cell(", ".join(item.get("evidence", [])))}'
            f'{render_text_cell(_render_retrieved_evidence(item.get("retrieved_evidence", [])))}'
            f'<td class="mono">{fmt_percent(item.get("evidence_hit"))}</td>'
            f'<td class="mono">{item.get("first_hit_rank")}</td>'
            "</tr>"
        )
    return (
        '<div class="table-wrap"><table><thead><tr>'
        '<th class="mono">Category</th><th>Question</th><th>Gold Evidence</th>'
        '<th>Retrieved Evidence</th><th class="mono">Hit</th><th class="mono">First Rank</th>'
        f"</tr></thead><tbody>{''.join(rows)}</tbody></table></div>"
    )


def estimate_tokens(text):
    text = str(text or "")
    ascii_words = len([part for part in text.split() if part])
    non_ascii_chars = sum(1 for char in text if ord(char) > 127)
    ascii_chars = sum(1 for char in text if ord(char) <= 127)
    return max(1, ascii_words + non_ascii_chars + ascii_chars // 4) if text else 0


def _render_retrieved_evidence(value):
    flattened = []
    for item in value[:10]:
        if isinstance(item, list):
            flattened.append("[" + ", ".join(str(part) for part in item) + "]")
        else:
            flattened.append(str(item))
    return ", ".join(flattened)


def mean(values):
    values = list(values)
    return sum(values) / len(values) if values else 0.0


if __name__ == "__main__":
    main()
