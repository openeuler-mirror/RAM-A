"""LongMemEval evaluation pipeline entry point for RAM-A.

Orchestrates preprocessing, add, search, evaluation, and report generation
for LongMemEval.

Usage:
    python evaluation/longmemeval/run.py [options]
"""

import argparse
import hashlib
import json
import os
import sys
from datetime import datetime
from pathlib import Path

# Ensure evaluation/ is on sys.path so common/ and longmemeval/ are importable.
EVALUATION_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, EVALUATION_ROOT)

from common.config import DATASET_DIR, OUTPUTS_DIR  # noqa: E402
from common.run_artifacts import git_hash as get_git_hash  # noqa: E402


def _model_slug(model: str) -> str:
    return model.rsplit("/", 1)[-1].replace("-", "").replace(".", "")


_DEFAULT_BASE_URL = "https://openrouter.ai/api/v1"


def _provider_slug(base_url: str) -> str | None:
    """Return a readable provider slug from base_url, or None if default."""
    if base_url.rstrip("/") == _DEFAULT_BASE_URL.rstrip("/"):
        return None
    from urllib.parse import urlparse
    parsed = urlparse(base_url)
    domain = parsed.netloc.lower()
    # e.g. "open.bigmodel.cn" → "open_bigmodel_cn", "api.siliconflow.cn" → "api_siliconflow_cn"
    slug = domain.replace(".", "_").replace("-", "_")
    return slug


def build_qa_tag(args) -> str:
    if getattr(args, "qa_output_tag", None):
        return args.qa_output_tag
    parts = []
    provider = _provider_slug(args.llm_base_url)
    if provider:
        parts.append(provider)
    parts.append(_model_slug(args.answerer_model))
    if args.judge_model != args.answerer_model:
        parts.append(_model_slug(args.judge_model))
    parts.append(args.answer_prompt_version)
    if args.memory_format != "full":
        parts.append(args.memory_format)
    parts.append(f"k{args.qa_top_k}")
    return "_".join(parts)


def file_sha256(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(8192), b""):
            h.update(chunk)
    return h.hexdigest()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Unified evaluation pipeline for RAM-A"
    )
    parser.add_argument(
        "--dataset-file",
        default="longmemeval_oracle.json",
        help="Dataset filename under data/longmemeval/ (default: longmemeval_oracle.json)",
    )
    parser.add_argument(
        "--embedding",
        default="openrouter",
        help="Embedding provider type (default: openrouter)",
    )
    parser.add_argument(
        "--backend",
        choices=["RAM-A"],
        default="RAM-A",
        help="Memory backend to evaluate (default: RAM-A)",
    )
    parser.add_argument(
        "--embedding-model",
        default="baai/bge-m3",
        help="Embedding model name (default: baai/bge-m3)",
    )
    parser.add_argument(
        "--dimensions",
        type=int,
        default=1024,
        help="Embedding dimensions (default: 1024)",
    )
    parser.add_argument(
        "--api-key-env",
        default="OPENROUTER_API_KEY",
        help="Environment variable holding the API key (default: OPENROUTER_API_KEY)",
    )
    parser.add_argument(
        "--embedding-batch-size",
        type=int,
        default=64,
        help="Number of texts per embedding batch for add/search (default: 64)",
    )
    parser.add_argument(
        "--resume",
        action="store_true",
        help="Skip steps whose output files already exist",
    )
    parser.add_argument(
        "--run-dir",
        default=None,
        help="Existing output directory to resume, or destination directory for a new run",
    )
    parser.add_argument(
        "--max-questions",
        type=int,
        default=None,
        help="Limit the number of LongMemEval questions for smoke tests",
    )
    parser.add_argument(
        "--phase",
        choices=["retrieval", "qa", "all"],
        default="retrieval",
        help="Pipeline phase to run (default: retrieval)",
    )
    parser.add_argument(
        "--retrieval-top-k",
        type=int,
        default=10,
        help="Top-k for search/retrieval phase (default: 10)",
    )
    parser.add_argument(
        "--answerer-model",
        default="openai/gpt-4o-mini",
        help="OpenAI-compatible chat model for answer generation",
    )
    parser.add_argument(
        "--judge-model",
        default="openai/gpt-4o-mini",
        help="OpenAI-compatible chat model for LLM-as-judge",
    )
    parser.add_argument(
        "--llm-api-key-env",
        default="OPENROUTER_API_KEY",
        help="Environment variable holding the chat LLM API key",
    )
    parser.add_argument(
        "--llm-base-url",
        default="https://openrouter.ai/api/v1",
        help="OpenAI-compatible chat completions base URL",
    )
    parser.add_argument(
        "--llm-thinking",
        choices=["default", "enabled", "disabled"],
        default="default",
        help="Optional provider thinking mode for models like GLM-5/5.1",
    )
    parser.add_argument(
        "--qa-top-k",
        type=int,
        default=10,
        help="Number of retrieved memories used for QA answer generation",
    )
    parser.add_argument(
        "--answer-prompt-version",
        choices=["lme_default"],
        default="lme_default",
        help="LongMemEval answer prompt version for QA (default: lme_default)",
    )
    parser.add_argument(
        "--memory-format",
        choices=["full", "compact"],
        default="full",
        help="Retrieved memory rendering format for QA (default: full)",
    )
    parser.add_argument(
        "--show-scores",
        action="store_true",
        help="Include retrieval scores in the answerer prompt (default: hidden)",
    )
    parser.add_argument(
        "--qa-output-tag",
        default=None,
        help="Override auto-generated QA output tag (default: auto from model/prompt/k)",
    )
    return parser.parse_args()


def latest_run_dir(parent: str, prefix: str) -> str | None:
    root = Path(parent)
    if not root.is_dir():
        return None
    matches = [path for path in root.iterdir() if path.is_dir() and path.name.endswith(prefix)]
    if not matches:
        return None
    return str(max(matches, key=lambda path: path.stat().st_mtime))


_EMPTY_METRICS = {
    "num_questions": 0,
    "num_missing_results": 0,
    "num_abstention_excluded": 0,
    "session": {"overall": {}, "by_type": {}},
    "turn": {"overall": {}, "by_type": {}},
}


def _validate_qa_meta(
    qa_meta_path: str,
    args: argparse.Namespace,
    prepared_path: str,
    search_results_path: str,
) -> bool:
    """Validate qa_meta artifact hashes and config. Returns True if resume is safe."""
    try:
        with open(qa_meta_path, "r", encoding="utf-8") as f:
            saved = json.load(f)
    except (json.JSONDecodeError, KeyError, OSError):
        print("[run] Warning: could not read qa_meta, re-running QA", file=sys.stderr)
        return False

    mismatches = []

    # Artifact hash checks
    current_prepared_hash = file_sha256(prepared_path)
    current_search_hash = file_sha256(search_results_path)
    if saved.get("prepared_sha256") != current_prepared_hash:
        mismatches.append("prepared.json (hash changed)")
    if saved.get("search_results_sha256") != current_search_hash:
        mismatches.append("search_results.json (hash changed)")

    # Config field checks
    _CONFIG_FIELDS = [
        "answerer_model",
        "judge_model",
        "llm_base_url",
        "llm_thinking",
        "qa_top_k",
        "answer_prompt_version",
        "memory_format",
        "show_scores",
    ]
    for field in _CONFIG_FIELDS:
        current = getattr(args, field, None)
        if field == "llm_thinking":
            current = None if current == "default" else current
        if field == "show_scores" and current is None:
            current = False
        if field not in saved:
            mismatches.append(f"{field}: missing from qa_meta")
            continue
        saved_val = saved.get(field)
        if saved_val != current:
            mismatches.append(f"{field}: saved={saved_val!r} current={current!r}")

    if mismatches:
        print(
            f"[run] qa_meta mismatch, re-running QA from scratch:\n"
            + "\n".join(f"  - {m}" for m in mismatches),
            file=sys.stderr,
        )
        return False

    return True


def main() -> None:
    args = parse_args()

    # --- Resolve paths ---
    dataset_dir = os.path.join(DATASET_DIR, "longmemeval")
    dataset_path = os.path.join(dataset_dir, args.dataset_file)

    if not os.path.isfile(dataset_path):
        print(
            f"[run] Dataset not found: {dataset_path}\n"
            f"[run] Please download the LongMemEval dataset and place it in {dataset_dir}/",
            file=sys.stderr,
        )
        sys.exit(1)

    dataset_basename = Path(args.dataset_file).stem
    model_slug = args.embedding_model.replace("/", "_")
    timestamp = datetime.now().strftime("%Y-%m-%dT%H%M%S")
    run_parent = os.path.join(OUTPUTS_DIR, "longmemeval")
    run_suffix = f"{model_slug}_{dataset_basename}"
    if args.run_dir:
        run_dir = args.run_dir
    elif args.resume:
        run_dir = latest_run_dir(run_parent, run_suffix) or os.path.join(
            run_parent, f"{timestamp}_{run_suffix}"
        )
    else:
        run_dir = os.path.join(run_parent, f"{timestamp}_{run_suffix}")
    os.makedirs(run_dir, exist_ok=True)

    # Paths for each pipeline stage
    prepared_path = os.path.join(run_dir, "prepared.json")
    store_path = Path(run_dir) / "store.jsonl"
    search_results_path = os.path.join(run_dir, "search_results.json")
    metrics_path = os.path.join(run_dir, "metrics.json")

    qa_tag = build_qa_tag(args)
    qa_results_path = os.path.join(run_dir, f"qa_results_{qa_tag}.json")
    qa_metrics_path = os.path.join(run_dir, f"qa_metrics_{qa_tag}.json")
    qa_meta_path = os.path.join(run_dir, f"qa_meta_{qa_tag}.json")
    report_path = os.path.join(run_dir, "report.html")
    error_report_path = os.path.join(run_dir, "errors.html")
    run_meta_path = os.path.join(run_dir, "run_meta.json")

    git_hash = get_git_hash()

    # --- Step 1: Preprocess ---
    # Lazy import to allow future extension with other datasets
    from longmemeval.preprocess import preprocess

    if args.resume and os.path.isfile(prepared_path):
        print("[run] Skipping preprocess (file exists)")
    else:
        print("[run] Preprocessing dataset...")
        preprocess(dataset_path, prepared_path, max_items=args.max_questions)

    metrics = None
    qa_metrics = None

    # Effective retrieval top-k: raise if qa needs more
    retrieval_top_k = max(args.retrieval_top_k, args.qa_top_k)
    if retrieval_top_k != args.retrieval_top_k:
        print(f"[run] Auto-raising retrieval top_k to {retrieval_top_k} for qa_top_k={args.qa_top_k}")

    if args.phase in ("retrieval", "all"):
        from common.backends import create_backend
        from common.backends.base import BackendConfig

        backend = create_backend(BackendConfig(
            name=args.backend,
            store_path=store_path,
            embedding=args.embedding,
            embedding_model=args.embedding_model,
            dimensions=args.dimensions,
            api_key_env=args.api_key_env,
            batch_size=args.embedding_batch_size,
            top_k=retrieval_top_k,
        ))

        # --- Step 2: Add memories to store ---
        if args.resume and backend.persists_local_store and store_path.exists():
            print("[run] Skipping add (store exists)")
        else:
            print("[run] Running add...")
            backend.add(Path(prepared_path))

        # --- Step 3: Search ---
        if args.resume and os.path.isfile(search_results_path):
            # Existing search results can be shorter than retrieval_top_k when
            # a scoped question has fewer available memories. Warn, but keep
            # resume cheap and deterministic.
            try:
                with open(search_results_path, "r", encoding="utf-8") as f:
                    existing_search = json.load(f)
                insufficient = [
                    item.get("query_id", f"#{i}")
                    for i, item in enumerate(existing_search)
                    if len(item.get("results", [])) < retrieval_top_k
                ]
                if insufficient:
                    print(
                        f"[run] {len(insufficient)} query(s) have fewer than {retrieval_top_k} "
                        "search results. Keeping existing search_results for resume.",
                        file=sys.stderr,
                    )
                print("[run] Skipping search (results exist)")
            except (json.JSONDecodeError, KeyError):
                print("[run] Warning: could not validate search results, re-running search")
                print("[run] Running search...")
                backend.search(Path(prepared_path), Path(search_results_path))
        else:
            print("[run] Running search...")
            backend.search(Path(prepared_path), Path(search_results_path))

        # --- Step 4: Evaluate retrieval ---
        from longmemeval.eval_retrieval import load_and_evaluate

        print("[run] Evaluating retrieval results...")
        metrics = load_and_evaluate(
            search_results_path,
            dataset_path,
            metrics_path,
            prepared_path=prepared_path,
        )
    elif os.path.isfile(metrics_path):
        with open(metrics_path, "r", encoding="utf-8") as f:
            metrics = json.load(f)

    if args.phase in ("qa", "all"):
        if not os.path.isfile(search_results_path):
            print(
                f"[run] Search results not found for QA: {search_results_path}\n"
                "[run] Run --phase retrieval first, or use --phase all.",
                file=sys.stderr,
            )
            sys.exit(1)

        # Search results can be shorter than qa_top_k when a scoped question has
        # fewer available memories. Warn so the run remains visible but do not fail.
        with open(search_results_path, "r", encoding="utf-8") as f:
            qa_search_data = json.load(f)
        insufficient = [
            item.get("query_id", f"#{i}")
            for i, item in enumerate(qa_search_data)
            if len(item.get("results", [])) < args.qa_top_k
        ]
        if insufficient:
            print(
                f"[run] {len(insufficient)} query(s) in search_results have fewer than "
                f"{args.qa_top_k} results. QA will use the available results.",
                file=sys.stderr,
            )

        # Validate qa_meta when resuming
        qa_resume = args.resume
        if args.resume:
            if os.path.isfile(qa_meta_path):
                qa_resume = _validate_qa_meta(
                    qa_meta_path, args, prepared_path, search_results_path,
                )
            else:
                print("[run] qa_meta not found, re-running QA from scratch", file=sys.stderr)
                qa_resume = False

        from longmemeval.eval_qa import load_and_evaluate_qa

        print("[run] Evaluating QA accuracy...")
        qa_metrics = load_and_evaluate_qa(
            search_results_path=search_results_path,
            prepared_path=prepared_path,
            output_results_path=qa_results_path,
            output_metrics_path=qa_metrics_path,
            answerer_model=args.answerer_model,
            judge_model=args.judge_model,
            llm_api_key_env=args.llm_api_key_env,
            llm_base_url=args.llm_base_url,
            llm_thinking=None if args.llm_thinking == "default" else args.llm_thinking,
            qa_top_k=args.qa_top_k,
            resume=qa_resume,
            answer_prompt_version=args.answer_prompt_version,
            memory_format=args.memory_format,
            show_scores=args.show_scores,
        )

        # Write QA-specific meta with artifact hashes
        qa_meta = {
            "answerer_model": args.answerer_model,
            "judge_model": args.judge_model,
            "llm_base_url": args.llm_base_url,
            "llm_thinking": None if args.llm_thinking == "default" else args.llm_thinking,
            "qa_top_k": args.qa_top_k,
            "retrieval_top_k": retrieval_top_k,
            "answer_prompt_version": args.answer_prompt_version,
            "memory_format": args.memory_format,
            "show_scores": args.show_scores,
            "prepared_sha256": file_sha256(prepared_path),
            "search_results_sha256": file_sha256(search_results_path),
            "qa_tag": qa_tag,
            "timestamp": timestamp,
        }
        with open(qa_meta_path, "w", encoding="utf-8") as f:
            json.dump(qa_meta, f, indent=2)
        print(f"[run] QA meta saved to {qa_meta_path}")

    if metrics is None:
        if qa_metrics is None:
            print(
                f"[run] No metrics available. Run --phase retrieval first.",
                file=sys.stderr,
            )
            sys.exit(1)
        print("[run] Warning: retrieval metrics not found, generating QA-only report")
        metrics = dict(_EMPTY_METRICS)

    # --- Step 5: Save run metadata ---
    run_meta = {
        "git_hash": git_hash,
        "dataset": args.dataset_file,
        "embedding_model": args.embedding_model,
        "backend": args.backend,
        "dimensions": args.dimensions,
        "embedding_type": args.embedding,
        "embedding_batch_size": args.embedding_batch_size,
        "retrieval_top_k": retrieval_top_k,
        "timestamp": timestamp,
        "max_questions": args.max_questions,
        "run_dir": run_dir,
        "phase": args.phase,
        "answerer_model": args.answerer_model if args.phase in ("qa", "all") else None,
        "judge_model": args.judge_model if args.phase in ("qa", "all") else None,
        "qa_top_k": args.qa_top_k if args.phase in ("qa", "all") else None,
        "answer_prompt_version": (
            args.answer_prompt_version if args.phase in ("qa", "all") else None
        ),
        "memory_format": args.memory_format if args.phase in ("qa", "all") else None,
    }
    with open(run_meta_path, "w", encoding="utf-8") as f:
        json.dump(run_meta, f, indent=2)
    print(f"[run] Run metadata saved to {run_meta_path}")

    # --- Step 6: Generate report ---
    from longmemeval.report import generate_longmemeval_error_report, generate_longmemeval_report

    print("[run] Generating report...")
    if qa_metrics is not None and os.path.exists(qa_results_path):
        with open(qa_results_path, "r", encoding="utf-8") as f:
            qa_results_for_errors = json.load(f)
        generate_longmemeval_error_report(
            qa_results_for_errors,
            error_report_path,
            run_meta=run_meta,
        )
    generate_longmemeval_report(
        metrics,
        report_path,
        dataset=dataset_basename,
        embedding_model=args.embedding_model,
        git_hash=git_hash,
        qa_metrics=qa_metrics,
        run_meta=run_meta,
        error_report_href="errors.html" if qa_metrics is not None else None,
    )

    print(f"[run] Done!")
    print(f"[run] Report:   {report_path}")
    if qa_metrics is not None:
        print(f"[run] Errors:   {error_report_path}")
    print(f"[run] Metrics:  {metrics_path}")
    if qa_metrics is not None:
        print(f"[run] QA Metrics: {qa_metrics_path}")


if __name__ == "__main__":
    main()
