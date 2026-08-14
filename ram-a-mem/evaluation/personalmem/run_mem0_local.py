#!/usr/bin/env python3
"""PersonaMem benchmark-prepared-v1 adapter for mem0 local SDK."""

from __future__ import annotations

import argparse
import json
import shutil
import sys
import time
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[2]
if str(REPO_ROOT) not in sys.path:
    sys.path.insert(0, str(REPO_ROOT))

from evaluation.clients.mem0_local import (  # noqa: E402
    Mem0LocalConfig,
    close_memory,
    create_memory,
    normalize_mem0_result,
)


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()

    if args.command == "ingest":
        return run_ingest(args)
    if args.command == "search":
        return run_search(args)

    parser.print_help()
    return 2


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Run PersonaMem benchmark-prepared-v1 with mem0 local SDK.",
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    ingest = subparsers.add_parser("ingest")
    add_common_args(ingest)
    ingest.add_argument("--rebuild", action="store_true", help="Delete work-dir before ingesting.")
    ingest.add_argument("--resume", action="store_true", help="Keep existing work-dir and append memories.")
    ingest.add_argument("--infer", action="store_true", help="Enable mem0 LLM inference during add.")
    ingest.add_argument("--limit-memories", type=int, default=0, help="Limit memories for smoke tests.")

    search = subparsers.add_parser("search")
    add_common_args(search)
    search.add_argument("--output", type=Path, required=True)
    search.add_argument("--top-k", type=int, default=10)
    search.add_argument("--threshold", type=float)
    search.add_argument("--limit-queries", type=int, default=0, help="Limit queries for smoke tests.")

    return parser


def add_common_args(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--dataset", type=Path, required=True)
    parser.add_argument("--work-dir", type=Path, required=True)
    parser.add_argument("--collection-name", required=True)
    parser.add_argument("--embedding-model", default="baai/bge-m3")
    parser.add_argument("--embedding-dims", type=int, default=1024)
    parser.add_argument("--llm-model", default="gpt-4o-mini")
    parser.add_argument("--api-key-env", default="OPENAI_API_KEY")
    parser.add_argument("--base-url")
    parser.add_argument("--on-disk", action="store_true")
    parser.add_argument("--max-retries", type=int, default=5)
    parser.add_argument("--retry-backoff-seconds", type=float, default=3.0)


def run_ingest(args: argparse.Namespace) -> int:
    if args.rebuild and args.resume:
        print("--rebuild and --resume cannot be used together", file=sys.stderr)
        return 2
    if args.rebuild and args.work_dir.exists():
        shutil.rmtree(args.work_dir)
    manifest_path = args.work_dir / "ingest_manifest.jsonl"
    completed_memory_ids = load_ingest_manifest(manifest_path) if args.resume else set()

    dataset = load_prepared_v1(args.dataset)
    memories = dataset["memories"]
    if args.limit_memories > 0:
        memories = memories[: args.limit_memories]
    total = len(memories)
    scope_count = count_memory_scopes(memories)

    print(
        f"mem0 ingest starting: total memories={total} scope_count={scope_count} "
        f"collection_name={args.collection_name} manifest={manifest_path}",
        flush=True,
    )
    if args.resume:
        print(
            f"mem0 ingest resume: loaded {len(completed_memory_ids)} completed memory ids",
            flush=True,
        )
    config = config_from_args(args)
    memory_client = create_memory(config)
    added = 0
    skipped = 0
    failed = 0
    scopes: set[str] = set()
    interval = progress_interval(total)

    try:
        for index, item in enumerate(memories, start=1):
            metadata = item.get("metadata")
            if not isinstance(metadata, dict):
                metadata = {}
            metadata = dict(metadata)

            scope_id = str(metadata.get("scope_id") or "")
            memory_id = str(item.get("id") or metadata.get("source_path") or "")
            text = str(item.get("text") or "")
            if args.resume and memory_id in completed_memory_ids:
                skipped += 1
                if should_log_progress(index, total, interval):
                    print(
                        f"mem0 ingest progress: {index}/{total} scope_id={scope_id} "
                        f"id={memory_id} skipped=existing",
                        flush=True,
                    )
                continue
            if not scope_id or not text:
                skipped += 1
                if should_log_progress(index, total, interval):
                    print(
                        f"mem0 ingest progress: {index}/{total} scope_id={scope_id} "
                        f"id={memory_id} skipped=1",
                        flush=True,
                    )
                continue

            role = metadata.get("role") or metadata.get("speaker") or "user"
            role = normalize_role(role)
            try:
                call_with_retries(
                    lambda: memory_client.add(
                        [{"role": role, "content": text}],
                        user_id=scope_id,
                        metadata=metadata,
                        infer=args.infer,
                    ),
                    max_retries=args.max_retries,
                    backoff_seconds=args.retry_backoff_seconds,
                    retry_log=lambda attempt, backoff, error: print(
                        f"mem0 ingest retry: id={memory_id} scope_id={scope_id} "
                        f"attempt={attempt} backoff={backoff:g} error={error!r}",
                        file=sys.stderr,
                        flush=True,
                    ),
                )
            except Exception as error:
                failed += 1
                print(
                    f"mem0 ingest failed: id={memory_id} scope_id={scope_id} "
                    f"error={error!r}",
                    file=sys.stderr,
                    flush=True,
                )
                raise
            scopes.add(scope_id)
            added += 1
            append_ingest_manifest(manifest_path, memory_id, scope_id)
            if should_log_progress(index, total, interval):
                print(
                    f"mem0 ingest progress: {index}/{total} scope_id={scope_id} "
                    f"id={memory_id}",
                    flush=True,
                )
    finally:
        close_memory(memory_client)

    print(
        json.dumps(
            {
                "command": "ingest",
                "dataset": str(args.dataset),
                "work_dir": str(args.work_dir),
                "collection_name": args.collection_name,
                "memories_seen": len(memories),
                "memories_added": added,
                "memories_skipped": skipped,
                "memories_failed": failed,
                "scope_count": len(scopes),
                "manifest_path": str(manifest_path),
                "infer": bool(args.infer),
            },
            ensure_ascii=False,
            indent=2,
        )
    )
    return 0


def run_search(args: argparse.Namespace) -> int:
    dataset = load_prepared_v1(args.dataset)
    queries = dataset["queries"]
    if args.limit_queries > 0:
        queries = queries[: args.limit_queries]
    total = len(queries)

    print(
        f"mem0 search starting: total queries={total} collection_name={args.collection_name} "
        f"top_k={args.top_k}",
        flush=True,
    )
    config = config_from_args(args)
    memory_client = create_memory(config)
    outputs = []
    empty_results = 0
    scope_mismatches = 0
    failed = 0
    interval = progress_interval(total)

    try:
        for index, query in enumerate(queries, start=1):
            filter_value = query.get("filter")
            if not isinstance(filter_value, dict):
                filter_value = {}
            scope_id = str(filter_value.get("scope_id") or "")
            query_text = str(query.get("text") or "")
            query_id = query.get("id")

            try:
                raw_results = call_with_retries(
                    lambda: search_memory(
                        memory_client,
                        query_text=query_text,
                        scope_id=scope_id,
                        top_k=args.top_k,
                        threshold=args.threshold,
                    ),
                    max_retries=args.max_retries,
                    backoff_seconds=args.retry_backoff_seconds,
                    retry_log=lambda attempt, backoff, error: print(
                        f"mem0 search retry: query_id={query_id} scope_id={scope_id} "
                        f"attempt={attempt} backoff={backoff:g} error={error!r}",
                        file=sys.stderr,
                        flush=True,
                    ),
                )
            except Exception as error:
                failed += 1
                print(
                    f"mem0 search failed: query_id={query_id} scope_id={scope_id} "
                    f"error={error!r}",
                    file=sys.stderr,
                    flush=True,
                )
                raise
            results = [normalize_mem0_result(item, scope_id) for item in raw_results]
            if not results:
                empty_results += 1
            scope_mismatches += count_scope_mismatches(results, scope_id)
            if should_log_progress(index, total, interval):
                print(
                    f"mem0 search progress: {index}/{total} query_id={query_id} "
                    f"scope_id={scope_id} results={len(results)}",
                    flush=True,
                )

            outputs.append(
                {
                    "query_path": f"$.queries[{index - 1}].text",
                    "query_id": query_id,
                    "query": query_text,
                    "filter": filter_value,
                    "metadata": query.get("metadata") if isinstance(query.get("metadata"), dict) else {},
                    "task": query.get("task") if isinstance(query.get("task"), dict) else {},
                    "results": results,
                }
            )
    finally:
        close_memory(memory_client)

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(outputs, ensure_ascii=False, indent=2), encoding="utf-8")
    print(
        json.dumps(
            {
                "command": "search",
                "dataset": str(args.dataset),
                "output": str(args.output),
                "collection_name": args.collection_name,
                "query_count": len(outputs),
                "top_k": args.top_k,
                "empty_result_count": empty_results,
                "scope_mismatch_count": scope_mismatches,
                "failed_query_count": failed,
            },
            ensure_ascii=False,
            indent=2,
        )
    )
    return 0


def search_memory(
    memory_client: Any,
    query_text: str,
    scope_id: str,
    top_k: int,
    threshold: float | None,
) -> list[Any]:
    attempts: list[dict[str, Any]] = [
        {"top_k": top_k, "filters": {"user_id": scope_id}},
        {"limit": top_k, "filters": {"user_id": scope_id}},
        {"user_id": scope_id, "top_k": top_k, "filters": {"user_id": scope_id}},
        {"user_id": scope_id, "limit": top_k, "filters": {"user_id": scope_id}},
    ]
    if threshold is not None:
        attempts = [dict(kwargs, threshold=threshold) for kwargs in attempts] + attempts

    last_signature_error: TypeError | ValueError | None = None
    for kwargs in attempts:
        try:
            response = memory_client.search(query_text, **kwargs)
            break
        except (TypeError, ValueError) as error:
            # mem0 releases differ on threshold/user_id/top_k/limit support.
            if isinstance(error, ValueError) and not is_mem0_search_signature_error(error):
                raise
            last_signature_error = error
    else:
        assert last_signature_error is not None
        raise last_signature_error

    if isinstance(response, dict):
        results = response.get("results", [])
    else:
        results = response
    return results if isinstance(results, list) else []


def is_mem0_search_signature_error(error: ValueError) -> bool:
    message = str(error)
    return (
        "Top-level entity parameters" in message
        or "not supported in search()" in message
        or "unexpected keyword" in message
    )


def call_with_retries(
    operation: Any,
    max_retries: int,
    backoff_seconds: float,
    retry_log: Any,
) -> Any:
    retries = max(0, max_retries)
    base_backoff = max(0.0, backoff_seconds)
    for attempt in range(retries + 1):
        try:
            return operation()
        except Exception as error:
            if attempt >= retries or not is_retryable_error(error):
                raise
            retry_number = attempt + 1
            backoff = base_backoff * retry_number
            retry_log(retry_number, backoff, error)
            if backoff > 0:
                time.sleep(backoff)
    raise RuntimeError("retry loop exited unexpectedly")


def is_retryable_error(error: Exception) -> bool:
    status_code = extract_status_code(error)
    if status_code in {400, 401, 402, 403}:
        return False
    if status_code in {429, 500, 502, 503, 504}:
        return True

    class_name = error.__class__.__name__
    module_name = getattr(error.__class__, "__module__", "")
    if module_name.startswith("openai") and class_name in {
        "APIConnectionError",
        "APITimeoutError",
    }:
        return True
    if module_name.startswith("httpx") and class_name in {
        "ConnectError",
        "ReadError",
        "TimeoutException",
    }:
        return True

    message = str(error)
    return (
        "UNEXPECTED_EOF" in message
        or "EOF occurred in violation of protocol" in message
        or "Connection error" in message
    )


def extract_status_code(error: Exception) -> int | None:
    status_code = getattr(error, "status_code", None)
    if isinstance(status_code, int):
        return status_code
    response = getattr(error, "response", None)
    status_code = getattr(response, "status_code", None)
    return status_code if isinstance(status_code, int) else None


def config_from_args(args: argparse.Namespace) -> Mem0LocalConfig:
    return Mem0LocalConfig(
        work_dir=args.work_dir,
        collection_name=args.collection_name,
        embedding_model=args.embedding_model,
        embedding_dims=args.embedding_dims,
        llm_model=args.llm_model,
        api_key_env=args.api_key_env,
        base_url=args.base_url,
        on_disk=args.on_disk,
    )


def load_prepared_v1(path: Path) -> dict[str, Any]:
    dataset = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(dataset, dict):
        raise ValueError("dataset must be a JSON object")
    if dataset.get("schema_version") != "benchmark-prepared-v1":
        raise ValueError("dataset schema_version must be benchmark-prepared-v1")
    memories = dataset.get("memories")
    queries = dataset.get("queries")
    if not isinstance(memories, list):
        raise ValueError("dataset.memories must be a list")
    if not isinstance(queries, list):
        raise ValueError("dataset.queries must be a list")
    return dataset


def count_memory_scopes(memories: list[Any]) -> int:
    scopes = set()
    for item in memories:
        if not isinstance(item, dict):
            continue
        metadata = item.get("metadata")
        if not isinstance(metadata, dict):
            continue
        scope_id = str(metadata.get("scope_id") or "")
        if scope_id:
            scopes.add(scope_id)
    return len(scopes)


def load_ingest_manifest(path: Path) -> set[str]:
    if not path.exists():
        return set()

    completed = set()
    with path.open("r", encoding="utf-8") as handle:
        for line_number, line in enumerate(handle, start=1):
            line = line.strip()
            if not line:
                continue
            try:
                item = json.loads(line)
            except json.JSONDecodeError as error:
                raise ValueError(f"invalid JSON in ingest manifest {path}:{line_number}: {error}") from error
            if not isinstance(item, dict):
                raise ValueError(f"ingest manifest entry must be an object at {path}:{line_number}")
            if item.get("status") != "ok":
                continue
            memory_id = item.get("id")
            if isinstance(memory_id, str) and memory_id:
                completed.add(memory_id)
    return completed


def append_ingest_manifest(path: Path, memory_id: str, scope_id: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    record = {
        "id": memory_id,
        "scope_id": scope_id,
        "status": "ok",
    }
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(record, ensure_ascii=False) + "\n")
        handle.flush()


def progress_interval(total: int) -> int:
    return 10 if total <= 100 else 50


def should_log_progress(index: int, total: int, interval: int) -> bool:
    return index == 1 or index == total or index % interval == 0


def normalize_role(value: Any) -> str:
    role = str(value or "user").strip().lower()
    if role in {"assistant", "system", "tool"}:
        return role
    return "user"


def count_scope_mismatches(results: list[dict[str, Any]], scope_id: str) -> int:
    count = 0
    for item in results:
        metadata = item.get("metadata")
        if not isinstance(metadata, dict):
            count += 1
            continue
        if str(metadata.get("scope_id") or "") != scope_id:
            count += 1
    return count


if __name__ == "__main__":
    sys.exit(main())
