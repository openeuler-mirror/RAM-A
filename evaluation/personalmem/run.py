#!/usr/bin/env python3
"""PersonaMem evaluation adapter for RAM-A.

The script shells out to the Rust `memory-bench` CLI for add/search, then scores
retrieval results against nearby gold fields in the dataset.
"""

from __future__ import annotations

import argparse
import ast
import csv
import hashlib
import http.client
import json
import os
import re
import shlex
import ssl
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable

from tqdm import tqdm

# Ensure evaluation/ is on sys.path so common/ is importable when this script is
# executed as `python evaluation/personalmem/run.py`.
EVALUATION_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(EVALUATION_ROOT))

from common.run_artifacts import (  # noqa: E402
    default_run_dir,
    ensure_dir,
    timestamp_run_id,
    write_run_meta,
)
from common.memory_ab import (  # noqa: E402
    canonical_sha256,
    ensure_run_mode,
    ensure_store_mode,
    file_sha256,
    validate_frozen_manifest,
    validate_memory_ab_preflight,
)
from common.memory_ab_stage import run_stage  # noqa: E402
from common.rust_memory_pipeline import (  # noqa: E402
    MemoryPipelineCommandConfig,
    build_memory_pipeline_command,
)


DEFAULT_TEXT_FIELDS = "text,content,message,memory"
DEFAULT_QUERY_FIELDS = "question,query"
DEFAULT_GOLD_FIELDS = "answer,ground_truth,gold,evidence,target"
DEFAULT_STORE = Path("data/personalmem.sqlite")
DEFAULT_OUTPUT = Path("outputs/personalmem_search_results.json")
DEFAULT_REPORT = Path("outputs/personalmem_report.json")
DEFAULT_RESPONSES = Path("outputs/personalmem_responses.json")
DEFAULT_GRADES = Path("outputs/personalmem_grades.json")
DEFAULT_CSV = Path("outputs/personalmem_results.csv")
DEFAULT_INDEXED_DATASET = Path("outputs/personalmem_extracted_prepared.json")
DEFAULT_EXTRACTION_MODEL = "openai/gpt-4o-mini"
DEFAULT_EXTRACTION_BASE_URL = "https://openrouter.ai/api/v1"
HF_REPO = "https://huggingface.co/datasets/bowen-upenn/PersonaMem/resolve/main"
RETRYABLE_HTTP_STATUS = {429, 500, 502, 503, 504}
RETRYABLE_API_EXCEPTIONS = (
    http.client.RemoteDisconnected,
    http.client.IncompleteRead,
    ssl.SSLError,
    urllib.error.URLError,
    TimeoutError,
    ConnectionResetError,
)


@dataclass
class QueryGold:
    query_path: str
    query: str
    gold_values: list[str]


@dataclass
class ChatCompletionResult:
    content: str
    attempts: int
    duration_ms: int


@dataclass
class AnswerInput:
    question_id: Any
    shared_context_id: Any
    question_type: Any
    topic: Any
    question: str
    all_options: list[str]
    correct_answer: Any
    prompt_question: dict[str, Any] | None
    errors: list[str]


class ChatCompletionError(RuntimeError):
    def __init__(
        self,
        message: str,
        error_type: str,
        attempts: int,
        duration_ms: int,
    ) -> None:
        super().__init__(message)
        self.error_type = error_type
        self.attempts = attempts
        self.duration_ms = duration_ms


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    governed_commands = {
        "add",
        "search",
        "eval",
        "answer",
        "grade",
        "pipeline",
        "official-pipeline",
        "memory-ab-pipeline",
    }
    if args.command in governed_commands:
        validate_experiment_args(args)
        implementation_digest = implementation_hash()
        promotion_policy_digest = (
            file_sha256(args.promotion_policy)
            if args.promotion_policy is not None
            else None
        )
        immutable = immutable_experiment_manifest(
            args,
            implementation_digest,
            promotion_policy_digest,
        )
        if args.phase == "full":
            validate_frozen_manifest(immutable, args.frozen_config)
        preflight_digest = (
            validate_memory_ab_preflight(
                args.preflight,
                "personalmem",
                implementation_digest,
            )
            if args.preflight is not None
            else None
        )
        args.implementation_hash = implementation_digest
        args.promotion_policy_hash = promotion_policy_digest
        args.configuration_hash = canonical_sha256(immutable)
        args.preflight_hash = preflight_digest
    apply_default_paths(args)
    if args.command in governed_commands and args.run_dir is not None:
        ensure_run_mode(args.run_dir, args.memory_mode)
    if args.command in governed_commands:
        ensure_store_mode(args.store, args.memory_mode)
    if args.command == "memory-ab-pipeline" and args.run_dir is not None:
        write_personalmem_arm_contract(args, immutable)

    if args.command == "download":
        return run_download(args)
    if args.command == "prepare":
        return run_prepare(args)
    if args.command == "add":
        args.dataset = resolve_indexed_dataset(args)
        return run_add(args)
    if args.command == "search":
        args.dataset = resolve_indexed_dataset(args)
        return run_search(args)
    if args.command == "eval":
        args.dataset = resolve_indexed_dataset(args)
        return run_eval(args)
    if args.command == "answer":
        args.dataset = resolve_indexed_dataset(args)
        return run_answer(args)
    if args.command == "grade":
        args.dataset = resolve_indexed_dataset(args)
        return run_grade(args)
    if args.command == "pipeline":
        args.dataset = resolve_indexed_dataset(args)
        add_code = run_add(args)
        if add_code != 0:
            return add_code
        search_code = run_search(args)
        if search_code != 0:
            return search_code
        return run_eval(args)
    if args.command == "official-pipeline":
        download_code = run_download(args)
        if download_code != 0:
            return download_code
        prepare_code = run_prepare(args)
        if prepare_code != 0:
            return prepare_code
        args.dataset = args.prepared_dataset
        args.dataset = resolve_indexed_dataset(args)
        add_code = run_add(args)
        if add_code != 0:
            return add_code
        search_code = run_search(args)
        if search_code != 0:
            return search_code
        return run_eval(args)
    if args.command == "memory-ab-pipeline":
        args.dataset = resolve_indexed_dataset(args)
        stages = [run_add, run_search, run_eval]
        if args.pipeline_phase == "all":
            stages.extend((run_answer, run_grade))
        for stage in stages:
            code = stage(args)
            if code != 0:
                return code
        return 0

    parser.print_help()
    return 2


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Run PersonaMem-style add/search/eval for RAM-A.",
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    for name in ("download", "prepare", "add", "search", "eval", "answer", "grade", "pipeline", "official-pipeline", "memory-ab-pipeline"):
        subparser = subparsers.add_parser(name)
        add_common_args(subparser)

    return parser


def add_common_args(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--dataset", type=Path)
    parser.add_argument("--store", default=DEFAULT_STORE, type=Path)
    parser.add_argument("--store-backend", default="sqlite", choices=("jsonl", "sqlite"))
    parser.add_argument("--output", default=DEFAULT_OUTPUT, type=Path)
    parser.add_argument("--report", default=DEFAULT_REPORT, type=Path)
    parser.add_argument("--html-report", type=Path, help="Optional HTML report path. Defaults to --report with .html suffix.")
    parser.add_argument("--responses", default=DEFAULT_RESPONSES, type=Path)
    parser.add_argument("--grades", default=DEFAULT_GRADES, type=Path)
    parser.add_argument("--csv", default=DEFAULT_CSV, type=Path)
    parser.add_argument("--run-dir", type=Path, help="Directory for standardized run artifacts.")
    parser.add_argument("--backend", default="RAM-A", help="Backend label to write into run_meta.json.")
    parser.add_argument("--answer-model", default="openai/gpt-4o-mini")
    parser.add_argument("--answer-base-url", default="https://openrouter.ai/api/v1")
    parser.add_argument("--answer-api-key-env", default="OPENROUTER_API_KEY")
    parser.add_argument(
        "--context-token-budget",
        default=2000,
        type=int,
        help=(
            "Approximate token budget for retrieved contexts in answer prompts. "
            "Set to 0 to keep full retrieved memories."
        ),
    )
    parser.add_argument("--max-retries", default=3, type=int)
    parser.add_argument("--retry-backoff-seconds", default=2.0, type=float)
    parser.add_argument("--resume", action="store_true")
    parser.add_argument("--top-k", default=10, type=int)
    parser.add_argument("--embedding", default="openrouter", choices=("openrouter", "hash"))
    parser.add_argument("--search-mode", default="hybrid", choices=("dense", "bm25", "hybrid"))
    parser.add_argument("--embedding-weight", default=0.7, type=float)
    parser.add_argument("--bm25-weight", default=0.3, type=float)
    parser.add_argument("--candidate-k", type=int)
    parser.add_argument("--model", default="baai/bge-m3")
    parser.add_argument("--dimensions", default=1024, type=int)
    parser.add_argument("--text-fields", default=DEFAULT_TEXT_FIELDS)
    parser.add_argument("--query-fields", default=DEFAULT_QUERY_FIELDS)
    parser.add_argument("--gold-fields", default=DEFAULT_GOLD_FIELDS)
    parser.add_argument(
        "--size",
        default="32k",
        choices=("32k", "128k", "1M"),
        help="Official PersonaMem context size to download/prepare.",
    )
    parser.add_argument(
        "--raw-dir",
        default=Path("data/personalmem/raw"),
        type=Path,
        help="Directory for downloaded PersonaMem files.",
    )
    parser.add_argument(
        "--prepared-dataset",
        default=Path("data/personalmem/prepared/personalmem_32k.json"),
        type=Path,
        help="JSON dataset generated from official CSV/JSONL files.",
    )
    parser.add_argument(
        "--schema-version",
        default="benchmark-prepared-v1",
        choices=("legacy", "benchmark-prepared-v1"),
        help="Deprecated compatibility option; prepare always writes benchmark-prepared-v1.",
    )
    parser.add_argument(
        "--limit-questions",
        default=0,
        type=int,
        help="Optional question limit for smoke tests. 0 means all questions.",
    )
    parser.add_argument(
        "--max-context-messages",
        default=0,
        type=int,
        help="Optional cap on context messages per shared context. 0 means all messages.",
    )
    parser.add_argument(
        "--cargo",
        default="cargo",
        help="Cargo executable or absolute path.",
    )
    parser.add_argument(
        "--repo-root",
        default=find_repo_root(),
        type=Path,
        help="RAM-A repository root.",
    )
    parser.add_argument(
        "--memory-mode",
        choices=("raw", "extracted"),
        default="raw",
        help="Memory representation indexed by this experiment arm.",
    )
    parser.add_argument(
        "--phase",
        choices=("pilot", "full"),
        default="pilot",
        help="Experiment governance phase.",
    )
    parser.add_argument(
        "--pipeline-phase",
        choices=("retrieval", "all"),
        default="all",
        help=(
            "Stages run by memory-ab-pipeline. retrieval omits live answer and "
            "grade stages; all preserves the default governed pair behavior."
        ),
    )
    parser.add_argument("--pair-id", default="standalone")
    parser.add_argument(
        "--indexed-dataset",
        default=DEFAULT_INDEXED_DATASET,
        type=Path,
        help="Prepared dataset produced and indexed by the extracted arm.",
    )
    parser.add_argument("--frozen-config", type=Path)
    parser.add_argument("--promotion-policy", type=Path)
    parser.add_argument("--preflight", type=Path)
    parser.add_argument("--extraction-model", default=DEFAULT_EXTRACTION_MODEL)
    parser.add_argument("--verifier-model", default=DEFAULT_EXTRACTION_MODEL)
    parser.add_argument("--extraction-api-key-env", default="OPENROUTER_API_KEY")
    parser.add_argument("--extraction-base-url", default=DEFAULT_EXTRACTION_BASE_URL)
    parser.add_argument("--extraction-cache-dir", type=Path)
    parser.add_argument("--extraction-cache-version")
    parser.add_argument("--max-candidate-tokens", type=int, default=320)
    parser.add_argument("--max-window-tokens", type=int, default=640)
    parser.add_argument("--context-before-messages", type=int, default=2)
    parser.add_argument("--context-after-messages", type=int, default=0)
    parser.add_argument("--extractor-responses", type=Path)
    parser.add_argument("--grounding-responses", type=Path)


def validate_experiment_args(args: argparse.Namespace) -> None:
    if args.phase == "full" and args.frozen_config is None:
        raise ValueError("--frozen-config is required for full runs")
    if args.phase == "full" and args.promotion_policy is None:
        raise ValueError("--promotion-policy is required for full runs")
    fixtures = (args.extractor_responses, args.grounding_responses)
    if any(value is not None for value in fixtures) and not all(
        value is not None for value in fixtures
    ):
        raise ValueError(
            "both --extractor-responses and --grounding-responses are required "
            "for offline fixture mode"
        )


def immutable_experiment_manifest(
    args: argparse.Namespace,
    implementation_digest: str,
    promotion_policy_digest: str | None,
) -> dict[str, Any]:
    """Return settings that must match across the paired memory arms."""
    return {
        "backend": args.backend,
        "store_backend": args.store_backend,
        "embedding": args.embedding,
        "embedding_model": args.model,
        "embedding_dimensions": args.dimensions,
        "search_mode": args.search_mode,
        "embedding_weight": args.embedding_weight,
        "bm25_weight": args.bm25_weight,
        "candidate_k": args.candidate_k,
        "top_k": args.top_k,
        "answer_model": args.answer_model,
        "answer_base_url": args.answer_base_url,
        "context_token_budget": args.context_token_budget,
        "max_retries": args.max_retries,
        "retry_backoff_seconds": args.retry_backoff_seconds,
        "text_fields": args.text_fields,
        "query_fields": args.query_fields,
        "gold_fields": args.gold_fields,
        "extraction_model": args.extraction_model,
        "verifier_model": args.verifier_model,
        "extraction_base_url": args.extraction_base_url,
        "max_candidate_tokens": args.max_candidate_tokens,
        "max_window_tokens": args.max_window_tokens,
        "context_before_messages": args.context_before_messages,
        "context_after_messages": args.context_after_messages,
        "pipeline_phase": args.pipeline_phase,
        "implementation_hash": implementation_digest,
        "promotion_policy_hash": promotion_policy_digest,
    }


def implementation_hash() -> str:
    """Hash the shared and PersonaMem implementation used by both arms."""
    project_root = EVALUATION_ROOT.parent
    roots = (
        EVALUATION_ROOT / "common",
        EVALUATION_ROOT / "personalmem",
        project_root / "crates" / "memory-bench" / "src",
        project_root / "crates" / "memory-core" / "src",
        project_root / "crates" / "memory-pipeline" / "src",
    )
    paths: list[Path] = []
    for root in roots:
        suffix = "*.rs" if root.name == "src" else "*.py"
        paths.extend(
            path
            for path in root.rglob(suffix)
            if not path.name.endswith("_test.py")
        )
    for manifest in (
        project_root / "Cargo.toml",
        project_root / "Cargo.lock",
        project_root / "crates" / "memory-pipeline" / "Cargo.toml",
    ):
        if manifest.is_file():
            paths.append(manifest)
    orchestrator = EVALUATION_ROOT / "scripts" / "run_memory_ab.py"
    if orchestrator.is_file():
        paths.append(orchestrator)
    binary_override = os.getenv("MEMORY_PIPELINE_BIN")
    if binary_override:
        binary_path = Path(shlex.split(binary_override)[0]).resolve()
        if binary_path.is_file():
            paths.append(binary_path)

    digest = hashlib.sha256()
    for path in sorted(paths):
        try:
            identity = str(path.relative_to(project_root))
        except ValueError:
            identity = f"MEMORY_PIPELINE_BIN:{path}"
        digest.update(identity.encode("utf-8"))
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def write_personalmem_arm_contract(
    args: argparse.Namespace,
    immutable: dict[str, Any],
) -> None:
    """Persist the auditable config and shared raw prepared input before stages."""
    source_path = Path(args.dataset)
    prepared = load_json(source_path)
    if not isinstance(prepared, dict) or prepared.get("schema_version") != "benchmark-prepared-v1":
        raise ValueError(
            "memory A/B runs require a benchmark-prepared-v1 PersonaMem dataset"
        )
    config = {
        "dataset": "personalmem",
        "source_path": str(source_path),
        "run_dir": str(args.run_dir),
        "run_id": Path(args.run_dir).name,
        "artifact_path": str(args.run_dir),
        "phase": args.phase,
        "memory_mode": args.memory_mode,
        "pair_id": args.pair_id,
        "source_hash": file_sha256(source_path),
        "configuration_hash": args.configuration_hash,
        "implementation_hash": args.implementation_hash,
        "promotion_policy_hash": args.promotion_policy_hash,
        "preflight_path": str(args.preflight) if args.preflight is not None else None,
        "preflight_hash": args.preflight_hash,
        **immutable,
    }
    _write_json_atomic(Path(args.run_dir) / "config.json", config)
    _write_json_atomic(Path(args.run_dir) / "raw_prepared.json", prepared)


def _write_json_atomic(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(
        json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n",
        encoding="utf-8",
    )
    temporary.replace(path)


def apply_default_paths(args: argparse.Namespace) -> None:
    """Route default artifacts into outputs/personalmem/<run_id> while preserving explicit paths."""
    if args.command in {"download", "prepare"}:
        return

    run_dir = args.run_dir
    if run_dir is None and has_all_default_artifact_paths(args):
        run_dir = default_run_dir(
            "personalmem",
            f"{timestamp_run_id()}_{args.memory_mode}",
        )
    if run_dir is None:
        return

    args.run_dir = ensure_dir(run_dir)
    if args.store == DEFAULT_STORE:
        args.store = args.run_dir / "store.sqlite"
    if args.output == DEFAULT_OUTPUT:
        args.output = args.run_dir / "search_results.json"
    if args.report == DEFAULT_REPORT:
        args.report = args.run_dir / "retrieval_metrics.json"
    if args.responses == DEFAULT_RESPONSES:
        args.responses = args.run_dir / "responses.json"
    if args.grades == DEFAULT_GRADES:
        args.grades = args.run_dir / "grade_metrics.json"
    if args.csv == DEFAULT_CSV:
        args.csv = args.run_dir / "grade_results.csv"
    if args.indexed_dataset == DEFAULT_INDEXED_DATASET:
        args.indexed_dataset = args.run_dir / "extracted_prepared.json"


def has_all_default_artifact_paths(args: argparse.Namespace) -> bool:
    return all(
        [
            args.store == DEFAULT_STORE,
            args.output == DEFAULT_OUTPUT,
            args.report == DEFAULT_REPORT,
            args.responses == DEFAULT_RESPONSES,
            args.grades == DEFAULT_GRADES,
            args.csv == DEFAULT_CSV,
        ]
    )


def run_download(args: argparse.Namespace) -> int:
    args.raw_dir.mkdir(parents=True, exist_ok=True)
    for name in persona_mem_filenames(args.size):
        url = f"{HF_REPO}/{name}"
        output = args.raw_dir / name
        if output.exists() and output.stat().st_size > 0:
            print(f"skip existing {output}")
            continue
        print(f"download {url} -> {output}")
        urllib.request.urlretrieve(url, output)
    return 0


def run_prepare(args: argparse.Namespace) -> int:
    questions_path = args.raw_dir / f"questions_{args.size}.csv"
    contexts_path = args.raw_dir / f"shared_contexts_{args.size}.jsonl"
    if not questions_path.exists() or not contexts_path.exists():
        print(
            "missing official PersonaMem files; run the download command first",
            file=sys.stderr,
        )
        return 1

    contexts = load_jsonl_contexts(contexts_path)
    questions = load_personamem_questions(questions_path, args.limit_questions)
    prepared = build_prepared_dataset(questions, contexts, args.max_context_messages)
    prepared = build_prepared_schema_v1(prepared, args.size)

    args.prepared_dataset.parent.mkdir(parents=True, exist_ok=True)
    args.prepared_dataset.write_text(
        json.dumps(prepared, ensure_ascii=False, indent=2),
        encoding="utf-8",
    )
    memory_count = len(prepared.get("memories", prepared.get("conversation", [])))
    query_count = len(prepared.get("queries", prepared.get("questions", [])))
    print(f"wrote prepared PersonaMem dataset to {args.prepared_dataset} ({memory_count} memories, {query_count} questions)")
    return 0


def resolve_indexed_dataset(args: argparse.Namespace) -> Path:
    """Return the prepared file consumed by the existing benchmark stages."""
    if args.dataset is None:
        raise ValueError("--dataset is required for pipeline")
    raw_dataset = Path(args.dataset)
    args.raw_dataset = raw_dataset
    if args.memory_mode == "raw":
        return raw_dataset

    prepared = load_json(raw_dataset)
    if not isinstance(prepared, dict) or prepared.get("schema_version") != "benchmark-prepared-v1":
        raise ValueError(
            "extracted memory mode requires a benchmark-prepared-v1 dataset"
        )
    indexed_dataset = Path(args.indexed_dataset)
    run_root = Path(args.run_dir) if getattr(args, "run_dir", None) else indexed_dataset.parent
    artifacts = run_root / "artifacts"
    configuration_hash = getattr(args, "configuration_hash", "personalmem-pilot-v1")
    config = MemoryPipelineCommandConfig(
        project_root=Path(getattr(args, "repo_root", find_repo_root())),
        cache_dir=(
            getattr(args, "extraction_cache_dir", None)
            or run_root / "cache" / "memory-pipeline"
        ),
        cache_version=(
            getattr(args, "extraction_cache_version", None)
            or configuration_hash
        ),
        model=getattr(args, "extraction_model", DEFAULT_EXTRACTION_MODEL),
        verifier_model=getattr(args, "verifier_model", DEFAULT_EXTRACTION_MODEL),
        api_key_env=getattr(args, "extraction_api_key_env", "OPENROUTER_API_KEY"),
        base_url=getattr(args, "extraction_base_url", DEFAULT_EXTRACTION_BASE_URL),
        extractor_responses=getattr(args, "extractor_responses", None),
        grounding_responses=getattr(args, "grounding_responses", None),
        max_candidate_tokens=getattr(args, "max_candidate_tokens", 320),
        max_window_tokens=getattr(args, "max_window_tokens", 640),
        context_before_messages=getattr(args, "context_before_messages", 2),
        context_after_messages=getattr(args, "context_after_messages", 0),
        episode_boundary_fields=("shared_context_id",),
        fail_fast=False,
    )
    command = build_memory_pipeline_command(
        config,
        raw_dataset,
        indexed_dataset,
        artifacts,
    )
    extraction_inputs = [raw_dataset]
    if config.extractor_responses is not None:
        extraction_inputs.extend(
            (config.extractor_responses, config.grounding_responses)
        )
    run_stage(
        "extract",
        command,
        (
            indexed_dataset,
            artifacts / "extraction_stats.json",
            artifacts / "run_metadata.json",
            artifacts / "prepared.json",
        ),
        {
            "configuration_hash": configuration_hash,
            "memory_mode": args.memory_mode,
            "pair_id": getattr(args, "pair_id", "standalone"),
        },
        inputs=tuple(extraction_inputs),
    )
    return indexed_dataset


def run_add(args: argparse.Namespace) -> int:
    if args.dataset is None:
        print("--dataset is required for add", file=sys.stderr)
        return 2
    command = bench_base_command(args) + [
        "add",
        "--dataset",
        str(args.dataset),
        "--text-fields",
        args.text_fields,
    ]
    return run_command(command, args.repo_root)


def run_search(args: argparse.Namespace) -> int:
    if args.dataset is None:
        print("--dataset is required for search", file=sys.stderr)
        return 2
    dataset = load_json(args.dataset)
    if is_personamem_prepared_dataset(dataset):
        return run_personamem_scoped_search(args, dataset)

    command = bench_base_command(args) + [
        "search",
        "--dataset",
        str(args.dataset),
        "--output",
        str(args.output),
        "--top-k",
        str(args.top_k),
        "--query-fields",
        args.query_fields,
    ]
    return run_command(command, args.repo_root)


def run_personamem_scoped_search(args: argparse.Namespace, dataset: Any) -> int:
    questions = dataset["questions"]
    args.output.parent.mkdir(parents=True, exist_ok=True)
    outputs = []
    total = len(questions)
    interval = answer_progress_interval(total)
    started = time.monotonic()
    print(f"[search] started | total={total}")
    progress_enabled = sys.stderr.isatty()

    with tempfile.TemporaryDirectory(
        prefix=f"{args.output.stem}_",
        dir=args.output.parent,
    ) as temp_dir:
        temp_dir_path = Path(temp_dir)
        iterator = tqdm(questions, total=total, desc="Searching PersonaMem queries") if progress_enabled else questions
        for index, question in enumerate(iterator):
            output_path = temp_dir_path / f"query_{index}.json"
            shared_context_id = str(question["shared_context_id"])
            command = bench_base_command(args) + [
                "search",
                "--query",
                str(question.get("question", "")),
                "--filter",
                json.dumps({"shared_context_id": shared_context_id}, ensure_ascii=False),
                "--output",
                str(output_path),
                "--top-k",
                str(args.top_k),
            ]
            code = run_command(command, args.repo_root, quiet=True)
            if code != 0:
                print(
                    f"[personalmem] search failed at {index + 1}/{total} "
                    f"shared_context_id={shared_context_id}",
                    file=sys.stderr,
                )
                print("+ " + " ".join(command), file=sys.stderr)
                return code

            result = load_json(output_path)
            if not isinstance(result, list) or not result:
                outputs.append(build_empty_personamem_search_output(index, question))
            elif not isinstance(result[0], dict):
                outputs.append(build_empty_personamem_search_output(index, question))
            else:
                outputs.append(enrich_personamem_search_output(index, question, result[0]))

            current = index + 1
            if not progress_enabled and should_log_answer_progress(current, total, interval):
                elapsed = time.monotonic() - started
                print(f"[search] {current}/{total} done | elapsed={elapsed:.1f}s", flush=True)

    args.output.write_text(
        json.dumps(outputs, ensure_ascii=False, indent=2),
        encoding="utf-8",
    )
    print(f"[search] wrote results to {args.output}")
    return 0


def is_personamem_prepared_dataset(dataset: Any) -> bool:
    if not isinstance(dataset, dict):
        return False
    questions = dataset.get("questions")
    if not isinstance(questions, list) or not questions:
        return False
    for question in questions:
        if not isinstance(question, dict):
            return False
        if not isinstance(question.get("question"), str):
            return False
        if not question.get("shared_context_id"):
            return False
    return True


def is_official_personamem_dataset(dataset: Any) -> bool:
    return (
        isinstance(dataset, dict)
        and dataset.get("source") == "bowen-upenn/PersonaMem"
        and isinstance(dataset.get("questions"), list)
    )


def build_empty_personamem_search_output(
    index: int,
    question: dict[str, Any],
) -> dict[str, Any]:
    return enrich_personamem_search_output(
        index,
        question,
        {
            "query": question.get("question", ""),
            "results": [],
        },
    )


def enrich_personamem_search_output(
    index: int,
    question: dict[str, Any],
    item: dict[str, Any],
) -> dict[str, Any]:
    return {
        "query_path": f"$.questions[{index}].question",
        "query": item.get("query", question.get("question", "")),
        "question_id": question.get("question_id"),
        "shared_context_id": question.get("shared_context_id"),
        "question_type": question.get("question_type"),
        "topic": question.get("topic"),
        "all_options": question.get("all_options"),
        "correct_answer": question.get("correct_answer"),
        "results": item.get("results", []),
    }


def run_eval(args: argparse.Namespace) -> int:
    if args.dataset is None:
        print("--dataset is required for eval", file=sys.stderr)
        return 2
    dataset = load_json(args.dataset)
    results = load_json(args.output)
    official_personamem = is_official_personamem_dataset(dataset)
    query_fields = split_csv(args.query_fields)
    gold_fields = split_csv(args.gold_fields)
    gold_by_path = {
        item.query_path: item
        for item in collect_query_gold(
            dataset,
            query_fields=query_fields,
            gold_fields=gold_fields,
        )
    }

    per_query = []
    scored = 0
    hits = 0
    reciprocal_rank_total = 0.0
    context_token_total = 0

    for result in results:
        query_path = result.get("query_path", "")
        query_gold = gold_by_path.get(query_path)
        retrieved = result.get("results", [])
        context_token_total += sum(estimate_tokens(item.get("text", "")) for item in retrieved)

        if official_personamem:
            per_query.append(
                {
                    "query_path": query_path,
                    "query": result.get("query", ""),
                    "has_gold": False,
                    "hit": None,
                    "rank": None,
                    "gold": [],
                    "scoring_method": "unsupported",
                }
            )
            continue

        if not query_gold or not query_gold.gold_values:
            per_query.append(
                {
                    "query_path": query_path,
                    "query": result.get("query", ""),
                    "has_gold": False,
                    "hit": False,
                    "rank": None,
                    "gold": [],
                }
            )
            continue

        scored += 1
        rank = first_match_rank(retrieved, query_gold.gold_values)
        hit = rank is not None
        if hit:
            hits += 1
            reciprocal_rank_total += 1.0 / rank

        per_query.append(
            {
                "query_path": query_path,
                "query": result.get("query", query_gold.query if query_gold else ""),
                "has_gold": True,
                "hit": hit,
                "rank": rank,
                "gold": query_gold.gold_values,
                "scoring_method": "answer_text",
            }
        )

    total_queries = len(results)
    hit_at_k = None if official_personamem else (hits / scored if scored else 0.0)
    mrr = None if official_personamem else (reciprocal_rank_total / scored if scored else 0.0)
    avg_context_tokens = context_token_total / total_queries if total_queries else 0.0

    report = {
        "report_type": "retrieval",
        "dataset": str(args.dataset),
        "output": str(args.output),
        "query_count": total_queries,
        "queries_with_gold": 0 if official_personamem else scored,
        "top_k": args.top_k,
        "hit_at_k": hit_at_k,
        "acc": hit_at_k,
        "mrr": mrr,
        "avg_context_tokens": avg_context_tokens,
        "embedding": args.embedding,
        "model": args.model if args.embedding == "openrouter" else args.embedding,
        "retrieval_scoring_supported": not official_personamem,
        "unsupported_reason": (
            "Official PersonaMem does not provide explicit evidence ids for retrieval Hit@K/MRR. "
            "Use answer/grade accuracy as the primary benchmark metric."
            if official_personamem else None
        ),
        "per_query": per_query,
    }

    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(
        json.dumps(report, ensure_ascii=False, indent=2),
        encoding="utf-8",
    )
    write_csv_summary(args.report.with_suffix(".csv"), report)
    html_report_path = stage_html_report_path(args, args.report)
    main_report_path = (args.run_dir / "report.html") if args.run_dir else html_report_path
    error_report_path = (args.run_dir / "errors.html") if args.run_dir else html_report_path.with_name("errors.html")
    from personalmem.report import generate_personamem_error_report, generate_personamem_main_report, generate_personamem_report

    run_meta = write_personamem_run_meta(args, phase="retrieval")
    generate_personamem_report(
        report,
        str(html_report_path),
        run_meta=run_meta,
    )
    generate_personamem_error_report(
        output_path=str(error_report_path),
        retrieval_report=report,
        run_meta=run_meta,
    )
    generate_personamem_main_report(
        output_path=str(main_report_path),
        retrieval_report=report,
        error_report_href="errors.html",
        run_meta=run_meta,
    )
    print(f"wrote PersonaMem report to {args.report}")
    print(f"wrote PersonaMem stage HTML report to {html_report_path}")
    print(f"wrote PersonaMem main HTML report to {main_report_path}")
    return 0


def run_answer(args: argparse.Namespace) -> int:
    if args.dataset is None:
        print("--dataset is required for answer", file=sys.stderr)
        return 2

    dataset = load_json(args.dataset)
    search_results = load_json(args.output)
    existing_responses = load_existing_responses(args.responses) if args.resume else {}
    responses = []
    total = len(search_results) if isinstance(search_results, list) else 0
    skipped = 0
    interval = answer_progress_interval(total)

    started = time.monotonic()
    print(f"[answer] started | total={total}")
    progress_enabled = sys.stderr.isatty()

    iterator = tqdm(search_results, total=total, desc="Answering PersonaMem queries") if progress_enabled else search_results
    for index, result in enumerate(iterator, start=1):
        query_path = result.get("query_path", "")
        existing_response = existing_responses.get(query_path)
        if should_skip_existing_response(existing_response):
            normalized = normalize_existing_response(existing_response)
            responses.append(normalized)
            skipped += 1
            if not progress_enabled and should_log_answer_progress(index, total, interval):
                elapsed = time.monotonic() - started
                print(f"[answer] {index}/{total} resume | skipped={skipped} | elapsed={elapsed:.1f}s", flush=True)
            continue

        answer_input = build_answer_input(result, dataset)
        errors = list(answer_input.errors)
        retrieved_contexts = build_retrieved_contexts(result.get("results", []))
        retrieved_contexts = apply_context_token_budget(
            retrieved_contexts,
            args.context_token_budget,
        )
        prompt = build_answer_prompt(answer_input.prompt_question, answer_input.all_options, retrieved_contexts)
        response = None
        predicted_answer = None
        parse_error = None
        answer_attempts = 0
        response_duration_ms = 0
        error_type = None

        if answer_input.question and answer_input.all_options and answer_input.correct_answer is not None:
            try:
                completion = call_chat_completion(
                    prompt=prompt,
                    model=args.answer_model,
                    base_url=args.answer_base_url,
                    api_key_env=args.answer_api_key_env,
                    max_retries=args.max_retries,
                    retry_backoff_seconds=args.retry_backoff_seconds,
                )
                response = completion.content
                answer_attempts = completion.attempts
                response_duration_ms = completion.duration_ms
                predicted_answer = parse_predicted_option(response)
                if predicted_answer is None:
                    parse_error = "failed to parse predicted answer from model response"
            except ChatCompletionError as error:
                answer_attempts = error.attempts
                response_duration_ms = error.duration_ms
                error_type = error.error_type
                errors.append(f"answer API call failed: {error}")
            except Exception as error:
                answer_attempts = 1
                error_type = type(error).__name__
                errors.append(f"answer API call failed: {error}")

        item = {
            "query_path": query_path,
            "question_id": answer_input.question_id,
            "shared_context_id": answer_input.shared_context_id,
            "question_type": answer_input.question_type,
            "topic": answer_input.topic,
            "question": answer_input.question,
            "all_options": answer_input.all_options,
            "correct_answer": answer_input.correct_answer,
            "retrieved_contexts": retrieved_contexts,
            "prompt": prompt,
            "model": args.answer_model,
            "response": response,
            "predicted_answer": predicted_answer,
            "answer_attempts": answer_attempts,
            "response_duration_ms": response_duration_ms,
        }
        if errors:
            item["error"] = "; ".join(errors)
        if error_type:
            item["error_type"] = error_type
        if parse_error:
            item["parse_error"] = parse_error
        responses.append(item)
        if not progress_enabled and should_log_answer_progress(index, total, interval):
            elapsed = time.monotonic() - started
            print(f"[answer] {index}/{total} done | skipped={skipped} | elapsed={elapsed:.1f}s", flush=True)

    args.responses.parent.mkdir(parents=True, exist_ok=True)
    args.responses.write_text(
        json.dumps(responses, ensure_ascii=False, indent=2),
        encoding="utf-8",
    )
    if args.resume:
        print(f"[answer] resume skipped={skipped}")
    print(f"[answer] wrote responses to {args.responses}")
    return 0


def answer_progress_interval(total: int) -> int:
    if total <= 100:
        return 10
    if total <= 1000:
        return 50
    return 100


def should_log_answer_progress(index: int, total: int, interval: int) -> bool:
    return index == 1 or index == total or index % interval == 0


def load_existing_responses(path: Path) -> dict[str, dict[str, Any]]:
    if not path.exists():
        return {}
    value = load_json(path)
    if not isinstance(value, list):
        return {}
    responses = {}
    for item in value:
        if not isinstance(item, dict):
            continue
        query_path = item.get("query_path")
        if isinstance(query_path, str):
            responses[query_path] = item
    return responses


def should_skip_existing_response(item: dict[str, Any] | None) -> bool:
    if not item:
        return False
    return item.get("response") is not None and item.get("predicted_answer") is not None


def normalize_existing_response(item: dict[str, Any]) -> dict[str, Any]:
    normalized = dict(item)
    normalized.setdefault("answer_attempts", 0)
    normalized.setdefault("response_duration_ms", 0)
    return normalized


def load_question_lookup(path: Path) -> dict[str, dict[str, Any]]:
    if not path.exists():
        return {}
    dataset = load_json(path)
    if not isinstance(dataset, dict):
        return {}
    questions = dataset.get("questions")
    if not isinstance(questions, list):
        return {}
    lookup = {}
    for index, question in enumerate(questions):
        if not isinstance(question, dict):
            continue
        query_path = f"$.questions[{index}].question"
        lookup[query_path] = question
        question_id = question.get("question_id")
        if question_id:
            lookup[str(question_id)] = question
    return lookup


def lookup_question_meta(item: dict[str, Any], lookup: dict[str, dict[str, Any]]) -> dict[str, Any]:
    question_id = item.get("question_id")
    if question_id is not None:
        found = lookup.get(str(question_id))
        if found:
            return found
    query_path = item.get("query_path")
    if isinstance(query_path, str):
        found = lookup.get(query_path)
        if found:
            return found
    return {}


def summarize_grade_groups(
    per_query: list[dict[str, Any]],
    field: str,
    context_token_budget: int,
) -> list[dict[str, Any]]:
    groups: dict[str, dict[str, Any]] = {}
    high_token_threshold = int(context_token_budget * 0.95) if context_token_budget > 0 else None
    for item in per_query:
        key = str(item.get(field) or "unknown")
        group = groups.setdefault(
            key,
            {
                "name": key,
                "total": 0,
                "correct": 0,
                "valid_predictions": 0,
                "context_token_total": 0.0,
                "wrong": 0,
                "wrong_near_token_budget": 0,
            },
        )
        tokens = float(item.get("estimated_context_tokens") or 0.0)
        is_correct = bool(item.get("is_correct"))
        group["total"] += 1
        group["correct"] += 1 if is_correct else 0
        group["valid_predictions"] += 1 if item.get("has_valid_prediction") else 0
        group["context_token_total"] += tokens
        if not is_correct:
            group["wrong"] += 1
            if high_token_threshold is not None and tokens >= high_token_threshold:
                group["wrong_near_token_budget"] += 1

    output = []
    for group in groups.values():
        total = int(group["total"])
        correct = int(group["correct"])
        output.append(
            {
                "name": group["name"],
                "total": total,
                "correct": correct,
                "wrong": int(group["wrong"]),
                "accuracy": correct / total if total else 0.0,
                "valid_predictions": int(group["valid_predictions"]),
                "avg_context_tokens": group["context_token_total"] / total if total else 0.0,
                "wrong_near_token_budget": int(group["wrong_near_token_budget"]),
            }
        )
    return sorted(output, key=lambda item: (-int(item["wrong"]), str(item["name"])))


def run_grade(args: argparse.Namespace) -> int:
    responses = load_json(args.responses)
    if not isinstance(responses, list):
        print("--responses must point to a JSON array", file=sys.stderr)
        return 2

    question_lookup = load_question_lookup(args.dataset) if args.dataset else {}
    per_query = []
    valid_predictions = 0
    correct = 0
    api_error_count = 0
    parse_error_count = 0
    retrieved_context_count_total = 0
    context_token_total = 0
    response_latency_total_ms = 0
    response_latency_count = 0

    for item in responses:
        if not isinstance(item, dict):
            continue

        predicted = normalize_option_label(item.get("predicted_answer"))
        expected = normalize_option_label(item.get("correct_answer"))
        option_lookup = option_lookup_from_list(item.get("all_options"))
        question_meta = lookup_question_meta(item, question_lookup)
        question_type = item.get("question_type") or question_meta.get("question_type") or "unknown"
        topic = item.get("topic") or question_meta.get("topic") or "unknown"
        is_valid = predicted is not None
        is_correct = is_valid and expected is not None and predicted == expected
        retrieved_contexts = item.get("retrieved_contexts", [])
        if not isinstance(retrieved_contexts, list):
            retrieved_contexts = []
        response_latency_ms = item.get("response_duration_ms")
        if response_latency_ms is not None:
            response_latency_total_ms += float(response_latency_ms)
            response_latency_count += 1

        valid_predictions += 1 if is_valid else 0
        correct += 1 if is_correct else 0
        api_error_count += 1 if item.get("error") else 0
        parse_error_count += 1 if item.get("parse_error") else 0
        retrieved_context_count_total += len(retrieved_contexts)
        context_token_total += sum(
            estimate_tokens(str(context.get("text", "")))
            for context in retrieved_contexts
            if isinstance(context, dict)
        )

        per_query.append(
            {
                "query_path": item.get("query_path"),
                "question_id": item.get("question_id"),
                "shared_context_id": item.get("shared_context_id"),
                "question_type": question_type,
                "topic": topic,
                "question": item.get("question"),
                "predicted_answer": predicted,
                "predicted_answer_text": option_lookup.get(predicted, predicted),
                "correct_answer": expected,
                "correct_answer_text": option_lookup.get(expected, expected),
                "is_correct": is_correct,
                "has_valid_prediction": is_valid,
                "retrieved_context_count": len(retrieved_contexts),
                "estimated_context_tokens": sum(
                    estimate_tokens(str(context.get("text", "")))
                    for context in retrieved_contexts
                    if isinstance(context, dict)
                ),
                "response_duration_ms": response_latency_ms,
                "error": item.get("error"),
                "parse_error": item.get("parse_error"),
            }
        )

    total = len(responses)
    by_question_type = summarize_grade_groups(per_query, "question_type", args.context_token_budget)
    by_topic = summarize_grade_groups(per_query, "topic", args.context_token_budget)
    summary = {
        "total": total,
        "valid_predictions": valid_predictions,
        "correct": correct,
        "answer_acc": correct / total if total else 0.0,
        "valid_answer_acc": correct / valid_predictions if valid_predictions else 0.0,
        "api_error_count": api_error_count,
        "parse_error_count": parse_error_count,
        "avg_retrieved_contexts": retrieved_context_count_total / total if total else 0.0,
        "avg_context_tokens": context_token_total / total if total else 0.0,
        "avg_response_latency_ms": (
            response_latency_total_ms / response_latency_count if response_latency_count else None
        ),
        "context_tokens_note": "estimated with a lightweight token-like heuristic",
        "context_token_budget": args.context_token_budget,
    }
    grades = {
        "report_type": "grade",
        "responses": str(args.responses),
        "summary": summary,
        "by_question_type": by_question_type,
        "by_topic": by_topic,
        "per_query": per_query,
    }

    args.grades.parent.mkdir(parents=True, exist_ok=True)
    args.grades.write_text(
        json.dumps(grades, ensure_ascii=False, indent=2),
        encoding="utf-8",
    )
    write_grade_csv(args.csv, per_query, summary)
    html_report_path = stage_html_report_path(args, args.grades)
    main_report_path = (args.run_dir / "report.html") if args.run_dir else html_report_path
    error_report_path = (args.run_dir / "errors.html") if args.run_dir else html_report_path.with_name("errors.html")
    from personalmem.report import generate_personamem_error_report, generate_personamem_main_report, generate_personamem_report

    run_meta = write_personamem_run_meta(args, phase="grade")
    generate_personamem_report(
        grades,
        str(html_report_path),
        run_meta=run_meta,
    )
    retrieval_report = load_optional_json(args.report)
    generate_personamem_error_report(
        output_path=str(error_report_path),
        retrieval_report=retrieval_report if isinstance(retrieval_report, dict) else None,
        grade_report=grades,
        run_meta=run_meta,
    )
    generate_personamem_main_report(
        output_path=str(main_report_path),
        retrieval_report=retrieval_report if isinstance(retrieval_report, dict) else None,
        grade_report=grades,
        error_report_href="errors.html",
        run_meta=run_meta,
    )
    print(f"wrote PersonaMem grades to {args.grades}")
    print(f"wrote PersonaMem grade CSV to {args.csv}")
    print(f"wrote PersonaMem grade stage HTML report to {html_report_path}")
    print(f"wrote PersonaMem main HTML report to {main_report_path}")
    return 0


def write_personamem_run_meta(args: argparse.Namespace, phase: str) -> dict[str, Any]:
    path = (args.run_dir / "run_meta.json") if args.run_dir else args.report.with_name("run_meta.json")
    return write_run_meta(
        path,
        dataset="personalmem",
        backend=getattr(args, "backend", "RAM-A"),
        phase=phase,
        dataset_path=str(args.dataset) if args.dataset else None,
        store=str(args.store),
        search_results=str(args.output),
        retrieval_metrics=str(args.report),
        retrieval_report=str(stage_html_report_path(args, args.report)),
        responses=str(args.responses),
        grade_metrics=str(args.grades),
        grade_report=str(stage_html_report_path(args, args.grades)),
        embedding=args.embedding,
        embedding_model=args.model if args.embedding == "openrouter" else args.embedding,
        store_backend=args.store_backend,
        search_mode=args.search_mode,
        embedding_weight=args.embedding_weight,
        bm25_weight=args.bm25_weight,
        candidate_k=args.candidate_k,
        dimensions=args.dimensions,
        top_k=args.top_k,
        answer_model=args.answer_model,
        context_token_budget=args.context_token_budget,
        memory_mode=getattr(args, "memory_mode", None),
        experiment_phase=getattr(args, "phase", None),
        pair_id=getattr(args, "pair_id", None),
        source_path=(
            str(args.raw_dataset)
            if getattr(args, "raw_dataset", None) is not None
            else None
        ),
        indexed_dataset=(
            str(args.dataset)
            if getattr(args, "dataset", None) is not None
            else None
        ),
        configuration_hash=getattr(args, "configuration_hash", None),
        implementation_hash=getattr(args, "implementation_hash", None),
        promotion_policy_hash=getattr(args, "promotion_policy_hash", None),
        preflight_hash=getattr(args, "preflight_hash", None),
    )


def stage_html_report_path(args: argparse.Namespace, metrics_path: Path) -> Path:
    if args.html_report:
        return args.html_report
    if args.run_dir:
        return args.run_dir / "stage_reports" / metrics_path.with_suffix(".html").name
    return metrics_path.with_suffix(".html")


def load_optional_json(path: Path) -> Any | None:
    try:
        if path.exists():
            return load_json(path)
    except (json.JSONDecodeError, OSError) as exc:
        print(f"warning: failed to load optional JSON {path}: {exc}", file=sys.stderr)
    return None


def bench_base_command(args: argparse.Namespace) -> list[str]:
    command = [
        args.cargo,
        "run",
        "-p",
        "memory-bench",
        "--",
        "--store",
        str(args.store),
        "--store-backend",
        args.store_backend,
        "--embedding",
        args.embedding,
        "--search-mode",
        args.search_mode,
        "--embedding-weight",
        str(args.embedding_weight),
        "--bm25-weight",
        str(args.bm25_weight),
        "--model",
        args.model,
        "--dimensions",
        str(args.dimensions),
    ]
    if args.candidate_k is not None:
        command.extend(["--candidate-k", str(args.candidate_k)])
    return command


def run_command(command: list[str], cwd: Path, *, quiet: bool = False) -> int:
    if not quiet:
        print("+ " + " ".join(command))
    completed = subprocess.run(
        command,
        cwd=cwd,
        stdout=subprocess.DEVNULL if quiet else None,
        stderr=subprocess.PIPE if quiet else None,
        text=True,
    )
    if quiet and completed.returncode != 0 and completed.stderr:
        print(completed.stderr.rstrip(), file=sys.stderr)
    return completed.returncode


def parse_question_index(query_path: str) -> int | None:
    prefix = "$.questions["
    suffix = "].question"
    if not query_path.startswith(prefix) or not query_path.endswith(suffix):
        return None
    raw_index = query_path[len(prefix):-len(suffix)]
    try:
        return int(raw_index)
    except ValueError:
        return None


def get_question(dataset: Any, index: int) -> dict[str, Any] | None:
    questions = dataset.get("questions") if isinstance(dataset, dict) else None
    if not isinstance(questions, list):
        return None
    if index < 0 or index >= len(questions):
        return None
    question = questions[index]
    return question if isinstance(question, dict) else None


def build_answer_input(result: dict[str, Any], dataset: Any) -> AnswerInput:
    if is_prepared_v1_search_result(result):
        return build_v1_answer_input(result)
    return build_legacy_answer_input(result, dataset)


def is_prepared_v1_search_result(result: dict[str, Any]) -> bool:
    task = result.get("task")
    return (
        "query_id" in result
        and isinstance(result.get("query"), str)
        and isinstance(task, dict)
        and ("answer_options" in task or "correct_answer" in task)
    )


def build_v1_answer_input(result: dict[str, Any]) -> AnswerInput:
    errors = []
    task = result.get("task")
    if not isinstance(task, dict):
        task = {}
        errors.append("task must be a JSON object for benchmark-prepared-v1 search result")

    options = []
    if "answer_options" not in task:
        errors.append("task.answer_options is missing")
    else:
        options, options_error = parse_all_options_list(task.get("answer_options"))
        if options_error:
            errors.append(f"task.answer_options: {options_error}")

    correct_answer = task.get("correct_answer")
    if correct_answer is None:
        errors.append("task.correct_answer is missing")

    question = result.get("query", "")
    metadata = result.get("metadata") if isinstance(result.get("metadata"), dict) else {}
    filter_value = result.get("filter") if isinstance(result.get("filter"), dict) else {}
    shared_context_id = metadata.get("shared_context_id") or filter_value.get("scope_id")
    prompt_question = {
        "question": question,
        "question_id": result.get("query_id"),
        "shared_context_id": shared_context_id,
        "correct_answer": correct_answer,
    }
    return AnswerInput(
        question_id=result.get("query_id"),
        shared_context_id=shared_context_id,
        question_type=metadata.get("question_type"),
        topic=metadata.get("topic"),
        question=question,
        all_options=options,
        correct_answer=correct_answer,
        prompt_question=prompt_question,
        errors=errors,
    )


def build_legacy_answer_input(result: dict[str, Any], dataset: Any) -> AnswerInput:
    errors = []
    query_path = result.get("query_path", "")
    question = None

    question_index = parse_question_index(query_path)
    if question_index is None:
        errors.append(f"failed to parse query_path: {query_path}")
    else:
        question = get_question(dataset, question_index)
        if question is None:
            errors.append(f"question index out of range: {question_index}")

    options = []
    if question is not None:
        options, options_error = parse_all_options_list(question.get("all_options", ""))
        if options_error:
            errors.append(options_error)

    return AnswerInput(
        question_id=question.get("question_id") if question else None,
        shared_context_id=question.get("shared_context_id") if question else None,
        question_type=question.get("question_type") if question else None,
        topic=question.get("topic") if question else None,
        question=question.get("question") if question else result.get("query", ""),
        all_options=options,
        correct_answer=question.get("correct_answer") if question else None,
        prompt_question=question,
        errors=errors,
    )


def parse_all_options_list(raw: Any) -> tuple[list[str], str | None]:
    if isinstance(raw, list):
        if all(isinstance(item, str) for item in raw):
            return raw, None
        return [], "all_options list contains non-string values"
    if not isinstance(raw, str) or not raw.strip():
        return [], "all_options is empty or not a string"
    try:
        value = ast.literal_eval(raw)
    except Exception as error:
        return [], f"failed to parse all_options: {error}"
    if not isinstance(value, list):
        return [], "all_options did not parse to a list"
    if not all(isinstance(item, str) for item in value):
        return [], "all_options list contains non-string values"
    return value, None


def build_retrieved_contexts(results: Any) -> list[dict[str, Any]]:
    if not isinstance(results, list):
        return []
    contexts = []
    for item in results:
        if not isinstance(item, dict):
            continue
        contexts.append(
            {
                "id": item.get("id"),
                "text": item.get("text", ""),
                "score": item.get("score"),
                "metadata": item.get("metadata", {}),
            }
        )
    return contexts


def apply_context_token_budget(
    retrieved_contexts: list[dict[str, Any]],
    token_budget: int,
) -> list[dict[str, Any]]:
    if token_budget <= 0 or not retrieved_contexts:
        return retrieved_contexts

    remaining_budget = token_budget
    remaining_contexts = len(retrieved_contexts)
    budgeted = []

    for context in retrieved_contexts:
        per_context_budget = max(1, remaining_budget // remaining_contexts)
        text = str(context.get("text", ""))
        trimmed_text = trim_text_to_token_budget(text, per_context_budget)
        item = dict(context)
        item["text"] = trimmed_text
        budgeted.append(item)
        remaining_budget = max(0, remaining_budget - estimate_tokens(trimmed_text))
        remaining_contexts -= 1

    return budgeted


def trim_text_to_token_budget(text: str, token_budget: int) -> str:
    if token_budget <= 0 or not text:
        return ""
    if estimate_tokens(text) <= token_budget:
        return text

    low = 0
    high = len(text)
    best = ""
    while low <= high:
        midpoint = (low + high) // 2
        candidate = text[:midpoint].rstrip()
        if estimate_tokens(candidate) <= token_budget:
            best = candidate
            low = midpoint + 1
        else:
            high = midpoint - 1

    return best


def build_answer_prompt(
    question: dict[str, Any] | None,
    all_options: list[str],
    retrieved_contexts: list[dict[str, Any]],
) -> str:
    question_text = question.get("question", "") if question else ""
    context_text = "\n\n".join(
        f"[Memory {index + 1}]\n{context.get('text', '')}"
        for index, context in enumerate(retrieved_contexts)
    )
    options_text = "\n".join(all_options)
    return (
        "You are answering a PersonaMem multiple-choice question.\n"
        "Use the retrieved memories as context and choose one option.\n\n"
        f"Question:\n{question_text}\n\n"
        f"Options:\n{options_text}\n\n"
        f"Retrieved memories:\n{context_text}\n\n"
        "Answer with only one option label, such as (a), (b), (c), or (d)."
    )


def call_chat_completion(
    prompt: str,
    model: str,
    base_url: str,
    api_key_env: str,
    max_retries: int,
    retry_backoff_seconds: float,
) -> ChatCompletionResult:
    api_key = os.environ.get(api_key_env)
    if not api_key:
        raise RuntimeError(f"missing API key env {api_key_env}")

    url = chat_completions_url(base_url)
    payload = {
        "model": model,
        "messages": [
            {
                "role": "system",
                "content": "You answer multiple-choice questions with only the option label.",
            },
            {
                "role": "user",
                "content": prompt,
            },
        ],
        "temperature": 0,
        "max_tokens": 32,
    }
    payload_bytes = json.dumps(payload).encode("utf-8")
    max_attempts = max(1, max_retries + 1)
    start_time = time.monotonic()

    for attempt in range(1, max_attempts + 1):
        try:
            request = urllib.request.Request(
                url,
                data=payload_bytes,
                headers={
                    "Authorization": f"Bearer {api_key}",
                    "Content-Type": "application/json",
                },
                method="POST",
            )
            with urllib.request.urlopen(request, timeout=120) as response:
                body = json.loads(response.read().decode("utf-8"))

            choices = body.get("choices")
            if not isinstance(choices, list) or not choices:
                raise ChatCompletionError(
                    f"chat completion response has no choices: {body}",
                    "invalid_response",
                    attempt,
                    elapsed_ms(start_time),
                )

            first = choices[0]
            if isinstance(first, dict):
                message = first.get("message")
                if isinstance(message, dict) and isinstance(message.get("content"), str):
                    return ChatCompletionResult(
                        content=message["content"].strip(),
                        attempts=attempt,
                        duration_ms=elapsed_ms(start_time),
                    )
                if isinstance(first.get("text"), str):
                    return ChatCompletionResult(
                        content=first["text"].strip(),
                        attempts=attempt,
                        duration_ms=elapsed_ms(start_time),
                    )

            raise ChatCompletionError(
                f"chat completion response has no text content: {body}",
                "invalid_response",
                attempt,
                elapsed_ms(start_time),
            )
        except urllib.error.HTTPError as error:
            body = read_http_error_body(error)
            message = f"chat completion returned HTTP {error.code}: {body}"
            if is_retryable_http_status(error.code) and attempt < max_attempts:
                sleep_before_retry(retry_backoff_seconds, attempt)
                continue
            raise ChatCompletionError(
                message,
                f"http_{error.code}",
                attempt,
                elapsed_ms(start_time),
            ) from error
        except RETRYABLE_API_EXCEPTIONS as error:
            if attempt < max_attempts:
                sleep_before_retry(retry_backoff_seconds, attempt)
                continue
            raise ChatCompletionError(
                str(error),
                type(error).__name__,
                attempt,
                elapsed_ms(start_time),
            ) from error

    raise ChatCompletionError(
        "chat completion failed without a response",
        "unknown",
        max_attempts,
        elapsed_ms(start_time),
    )


def chat_completions_url(base_url: str) -> str:
    base = base_url.rstrip("/")
    if base.endswith("/chat/completions"):
        return base
    return f"{base}/chat/completions"


def read_http_error_body(error: urllib.error.HTTPError) -> str:
    try:
        return error.read().decode("utf-8", errors="replace")
    except Exception:
        return ""


def is_retryable_http_status(status: int) -> bool:
    return status in RETRYABLE_HTTP_STATUS


def sleep_before_retry(backoff_seconds: float, attempt: int) -> None:
    delay = max(0.0, backoff_seconds) * attempt
    if delay > 0:
        time.sleep(delay)


def elapsed_ms(start_time: float) -> int:
    return int((time.monotonic() - start_time) * 1000)


def parse_predicted_option(response: str | None) -> str | None:
    if not response:
        return None
    match = re.search(r"\(([a-dA-D])\)", response)
    if match:
        return f"({match.group(1).lower()})"

    stripped = response.strip()
    if re.fullmatch(r"[a-dA-D]", stripped):
        return f"({stripped.lower()})"

    match = re.search(r"(?:answer is|答案是)\s*([a-dA-D])\b", response, re.IGNORECASE)
    if match:
        return f"({match.group(1).lower()})"

    return None


def normalize_option_label(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    return parse_predicted_option(value)


def write_grade_csv(
    path: Path,
    per_query: list[dict[str, Any]],
    summary: dict[str, Any],
) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", newline="", encoding="utf-8") as handle:
        fieldnames = [
            "row_type",
            "query_path",
            "question_id",
            "shared_context_id",
            "question_type",
            "topic",
            "predicted_answer",
            "predicted_answer_text",
            "correct_answer",
            "correct_answer_text",
            "is_correct",
            "has_valid_prediction",
            "retrieved_context_count",
            "estimated_context_tokens",
            "response_duration_ms",
            "error",
            "parse_error",
            "total",
            "valid_predictions",
            "correct",
            "answer_acc",
            "valid_answer_acc",
            "api_error_count",
            "parse_error_count",
            "avg_retrieved_contexts",
            "avg_context_tokens",
        ]
        writer = csv.DictWriter(handle, fieldnames=fieldnames)
        writer.writeheader()
        for item in per_query:
            writer.writerow(
                {
                    "row_type": "query",
                    "query_path": item.get("query_path"),
                    "question_id": item.get("question_id"),
                    "shared_context_id": item.get("shared_context_id"),
                    "question_type": item.get("question_type"),
                    "topic": item.get("topic"),
                    "predicted_answer": item.get("predicted_answer"),
                    "predicted_answer_text": item.get("predicted_answer_text"),
                    "correct_answer": item.get("correct_answer"),
                    "correct_answer_text": item.get("correct_answer_text"),
                    "is_correct": item.get("is_correct"),
                    "has_valid_prediction": item.get("has_valid_prediction"),
                    "retrieved_context_count": item.get("retrieved_context_count"),
                    "estimated_context_tokens": item.get("estimated_context_tokens"),
                    "response_duration_ms": item.get("response_duration_ms"),
                    "error": item.get("error"),
                    "parse_error": item.get("parse_error"),
                }
            )
        writer.writerow(
            {
                "row_type": "summary",
                "total": summary.get("total"),
                "valid_predictions": summary.get("valid_predictions"),
                "correct": summary.get("correct"),
                "answer_acc": summary.get("answer_acc"),
                "valid_answer_acc": summary.get("valid_answer_acc"),
                "api_error_count": summary.get("api_error_count"),
                "parse_error_count": summary.get("parse_error_count"),
                "avg_retrieved_contexts": summary.get("avg_retrieved_contexts"),
                "avg_context_tokens": summary.get("avg_context_tokens"),
            }
        )


def persona_mem_filenames(size: str) -> list[str]:
    return [f"questions_{size}.csv", f"shared_contexts_{size}.jsonl"]


def load_personamem_questions(path: Path, limit: int) -> list[dict[str, Any]]:
    questions: list[dict[str, Any]] = []
    with path.open("r", newline="", encoding="utf-8") as handle:
        for row in csv.DictReader(handle):
            questions.append(row)
            if limit and len(questions) >= limit:
                break
    return questions


def load_jsonl_contexts(path: Path) -> dict[str, Any]:
    contexts: dict[str, Any] = {}
    with path.open("r", encoding="utf-8") as handle:
        for line_number, line in enumerate(handle, start=1):
            line = line.strip()
            if not line:
                continue
            value = json.loads(line)
            context_id = infer_context_id(value, line_number)
            contexts[context_id] = value
    return contexts


def infer_context_id(value: Any, line_number: int) -> str:
    if isinstance(value, dict):
        for key in ("shared_context_id", "context_id", "id"):
            if key in value:
                return str(value[key])
        if len(value) == 1:
            return str(next(iter(value.keys())))
    return str(line_number - 1)


def build_prepared_dataset(
    questions: list[dict[str, Any]],
    contexts: dict[str, Any],
    max_context_messages: int,
) -> dict[str, Any]:
    conversation = []
    seen_memory_ids = set()
    prepared_questions = []

    for row in questions:
        context_id = row.get("shared_context_id", "")
        context = contexts.get(context_id)
        end_index = parse_optional_int(row.get("end_index_in_shared_context"))
        messages = extract_context_messages(context, end_index)
        if max_context_messages:
            messages = messages[-max_context_messages:]

        for index, message in enumerate(messages):
            memory_id = f"{context_id}:{index}"
            if memory_id in seen_memory_ids:
                continue
            seen_memory_ids.add(memory_id)
            conversation.append(
                {
                    "id": memory_id,
                    "shared_context_id": context_id,
                    "speaker": message.get("role", ""),
                    "text": message["text"],
                }
            )

        question = row.get("user_question_or_message", "")
        correct_answer = row.get("correct_answer", "")
        all_options = row.get("all_options", "")
        prepared_questions.append(
            {
                "question_id": row.get("question_id", ""),
                "shared_context_id": context_id,
                "question_type": row.get("question_type", ""),
                "topic": row.get("topic", ""),
                "question": question,
                "answer": option_text(correct_answer, all_options),
                "correct_answer": correct_answer,
                "all_options": all_options,
            }
        )

    return {
        "source": "bowen-upenn/PersonaMem",
        "conversation": conversation,
        "questions": prepared_questions,
    }


def build_prepared_schema_v1(prepared: dict[str, Any], split: str) -> dict[str, Any]:
    memories = []
    for index, memory in enumerate(prepared.get("conversation", [])):
        if not isinstance(memory, dict):
            continue
        shared_context_id = str(memory.get("shared_context_id", ""))
        speaker = str(memory.get("speaker", ""))
        memory_id = memory.get("id") or f"{shared_context_id}:{index}"
        memories.append(
            {
                "id": str(memory_id),
                "text": str(memory.get("text", "")),
                "metadata": {
                    "dataset": "personamem",
                    "scope_id": shared_context_id,
                    "shared_context_id": shared_context_id,
                    "role": speaker,
                    "speaker": speaker,
                    "turn_index": index,
                    "conversation_index": index,
                    "source_path": f"$.conversation[{index}].text",
                },
            }
        )

    queries = []
    for index, question in enumerate(prepared.get("questions", [])):
        if not isinstance(question, dict):
            continue
        shared_context_id = str(question.get("shared_context_id", ""))
        answer_options, options_error = parse_all_options_list(question.get("all_options"))
        metadata = {
            "shared_context_id": shared_context_id,
            "question_type": question.get("question_type"),
            "topic": question.get("topic"),
            "answer": question.get("answer"),
            "query_path": f"$.questions[{index}].question",
        }
        if options_error:
            metadata["all_options_parse_warning"] = options_error
        queries.append(
            {
                "id": str(question.get("question_id", "")),
                "text": str(question.get("question", "")),
                "filter": {
                    "scope_id": shared_context_id,
                },
                "metadata": metadata,
                "task": {
                    "type": "multiple_choice",
                    "answer_options": answer_options,
                    "correct_answer": question.get("correct_answer"),
                },
            }
        )

    return {
        "schema_version": "benchmark-prepared-v1",
        "dataset": {
            "name": "personamem",
            "split": split,
            "source": "bowen-upenn/PersonaMem",
        },
        "memories": memories,
        "queries": queries,
    }


def extract_context_messages(context: Any, end_index: int | None) -> list[dict[str, str]]:
    if context is None:
        return []
    value = context
    if isinstance(value, dict) and len(value) == 1:
        value = next(iter(value.values()))
    if isinstance(value, dict):
        for key in ("messages", "conversation", "context", "shared_context"):
            if key in value:
                value = value[key]
                break
    if isinstance(value, str):
        return [{"role": "context", "text": value[:end_index] if end_index else value}]
    if not isinstance(value, list):
        return [{"role": "context", "text": json.dumps(value, ensure_ascii=False)}]

    messages = value[:end_index] if end_index else value
    output = []
    for item in messages:
        role, text = extract_message_text(item)
        if text:
            output.append({"role": role, "text": text})
    return output


def extract_message_text(item: Any) -> tuple[str, str]:
    if isinstance(item, str):
        return "context", item.strip()
    if isinstance(item, dict):
        role = str(item.get("role") or item.get("speaker") or item.get("name") or "")
        for key in ("content", "text", "message"):
            if isinstance(item.get(key), str):
                return role, item[key].strip()
        return role, json.dumps(item, ensure_ascii=False)
    return "context", str(item).strip()


def option_text(correct_answer: str, all_options: str) -> str:
    if not correct_answer:
        return ""
    options = parse_options(all_options)
    return options.get(correct_answer.strip().lower(), correct_answer)


def parse_options(raw: str) -> dict[str, str]:
    try:
        value = ast.literal_eval(raw)
    except Exception:
        return {}
    if not isinstance(value, list):
        return {}
    output = {}
    for option in value:
        if not isinstance(option, str):
            continue
        label = parse_option_label(option)
        if label:
            output[label] = option
    return output


def parse_optional_int(value: Any) -> int | None:
    try:
        if value in (None, ""):
            return None
        return int(value)
    except (TypeError, ValueError):
        return None


def collect_query_gold(
    value: Any,
    query_fields: list[str],
    gold_fields: list[str],
    path: str = "$",
) -> Iterable[QueryGold]:
    if isinstance(value, dict):
        query_values = [
            str(value[field]).strip()
            for field in query_fields
            if isinstance(value.get(field), str) and str(value[field]).strip()
        ]
        if query_values:
            gold_values = collect_gold_values(value, gold_fields)
            for query in query_values:
                yield QueryGold(
                    query_path=f"{path}.{matching_field(value, query_fields, query)}",
                    query=query,
                    gold_values=gold_values,
                )

        for key, child in value.items():
            yield from collect_query_gold(child, query_fields, gold_fields, f"{path}.{key}")
    elif isinstance(value, list):
        for index, child in enumerate(value):
            yield from collect_query_gold(child, query_fields, gold_fields, f"{path}[{index}]")


def matching_field(value: dict[str, Any], fields: list[str], text: str) -> str:
    for field in fields:
        if value.get(field) == text:
            return field
    return "query"


def collect_gold_values(value: Any, gold_fields: list[str]) -> list[str]:
    output: list[str] = []
    if isinstance(value, dict):
        for key, child in value.items():
            if key in gold_fields:
                output.extend(flatten_strings(child))
    return dedupe([item.strip() for item in output if item.strip()])


def flatten_strings(value: Any) -> list[str]:
    if isinstance(value, str):
        return [value]
    if isinstance(value, list):
        output: list[str] = []
        for item in value:
            output.extend(flatten_strings(item))
        return output
    if isinstance(value, dict):
        output = []
        for item in value.values():
            output.extend(flatten_strings(item))
        return output
    return []


def first_match_rank(results: list[dict[str, Any]], gold_values: list[str]) -> int | None:
    normalized_gold = [normalize_text(value) for value in gold_values if normalize_text(value)]
    for index, result in enumerate(results, start=1):
        text = normalize_text(result.get("text", ""))
        # Keep the match one-way to avoid false positives from very short retrieved
        # snippets being substrings of a longer gold answer.
        if any(gold in text for gold in normalized_gold):
            return index
    return None


def option_lookup_from_list(raw: Any) -> dict[str, str]:
    if not isinstance(raw, list):
        return {}
    output = {}
    for option in raw:
        if not isinstance(option, str):
            continue
        label = parse_option_label(option)
        if label:
            output[label] = option.strip()
    return output


def parse_option_label(option: str) -> str | None:
    match = re.match(r"^\(?([A-Za-z0-9]+)[\).]\s*", option.strip())
    if not match:
        return None
    return f"({match.group(1).lower()})"


def estimate_tokens(text: str) -> int:
    # Lightweight approximation for dashboards before model-specific tokenizers are added.
    ascii_words = len([part for part in text.split() if part])
    non_ascii_chars = sum(1 for char in text if ord(char) > 127)
    ascii_chars = sum(1 for char in text if ord(char) <= 127)
    return max(1, ascii_words + non_ascii_chars + ascii_chars // 4) if text else 0


def write_csv_summary(path: Path, report: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(
            handle,
            fieldnames=[
                "dataset",
                "model",
                "top_k",
                "query_count",
                "queries_with_gold",
                "acc",
                "hit_at_k",
                "mrr",
                "avg_context_tokens",
            ],
        )
        writer.writeheader()
        writer.writerow(
            {
                "dataset": report["dataset"],
                "model": report["model"],
                "top_k": report["top_k"],
                "query_count": report["query_count"],
                "queries_with_gold": report["queries_with_gold"],
                "acc": report["acc"],
                "hit_at_k": report["hit_at_k"],
                "mrr": report["mrr"],
                "avg_context_tokens": report["avg_context_tokens"],
            }
        )


def load_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def split_csv(value: str) -> list[str]:
    return [item.strip() for item in value.split(",") if item.strip()]


def normalize_text(value: Any) -> str:
    return str(value).strip().lower()


def dedupe(values: list[str]) -> list[str]:
    seen = set()
    output = []
    for value in values:
        if value not in seen:
            seen.add(value)
            output.append(value)
    return output


def find_repo_root() -> Path:
    return Path(__file__).resolve().parents[2]


if __name__ == "__main__":
    sys.exit(main())
