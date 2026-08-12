"""Short configuration-driven entrypoint for the three benchmark runners."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
from typing import Any

from common.benchmark_config import load_benchmark_config
from common.memory_ab import canonical_sha256, file_sha256


def load_runtime_config(path: Path, dataset: str) -> dict[str, Any]:
    config = load_benchmark_config(path, dataset)
    run = config["run"]
    config_dir = Path(config["config_path"]).parent
    output_root = _resolve_path(run.get("output_root", "outputs/memory-ab"), config_dir)
    policy = run.get("promotion_policy")
    policy_path = _resolve_path(str(policy), config_dir) if policy else None
    if run.get("mode", "normal") == "strict":
        if policy_path is None or not policy_path.is_file():
            raise ValueError("run.promotion_policy is required in strict mode")
    elif policy_path is not None:
        raise ValueError("run.promotion_policy is only valid in strict mode")
    config["output_root"] = output_root
    config["promotion_policy_path"] = policy_path
    pair_dir = output_root / dataset / str(run.get("phase", "full")) / str(
        run.get("pair_id", "benchmark")
    )
    manifest = {
        "dataset": dataset,
        "phase": run.get("phase", "full"),
        "mode": run.get("mode", "normal"),
        "pair_id": run.get("pair_id", "benchmark"),
        "config_path": str(config["config_path"]),
        "config_hash": canonical_sha256(_manifest_value(config)),
        "dataset_path": str(config["dataset_file"]),
        "dataset_hash": file_sha256(config["dataset_file"]),
        "promotion_policy_hash": file_sha256(policy_path) if policy_path else None,
    }
    manifest_path = pair_dir / "run_manifest.json"
    manifest_path.parent.mkdir(parents=True, exist_ok=True)
    manifest_path.write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    config["manifest_path"] = manifest_path
    return config


def build_memory_ab_command(config: dict[str, Any]) -> tuple[list[str], dict[str, str]]:
    run = config["run"]
    dataset = str(config["dataset_name"])
    command = [
        sys.executable,
        "-m",
        "scripts.run_memory_ab",
        "--dataset",
        dataset,
        "--phase",
        str(run.get("phase", "full")),
        "--mode",
        str(run.get("mode", "normal")),
        "--execution",
        str(run.get("execution", "ab")),
        "--pair-id",
        str(run.get("pair_id", "benchmark")),
        "--dataset-file",
        str(config["dataset_file"]),
        "--output-root",
        str(config["output_root"]),
    ]
    if run.get("execution", "ab") == "single":
        command.extend(["--memory-mode", str(run["memory_mode"])])
    if config["promotion_policy_path"] is not None:
        command.extend(["--promotion-policy", str(config["promotion_policy_path"])])
    env = dict(os.environ)
    embedding = config.get("embedding", {})
    env.setdefault(
        str(embedding.get("api_key_env", "OPENROUTER_API_KEY")),
        "",
    )
    forwarded = _build_forwarded_args(config)
    if dataset == "locomo":
        env.update(_locomo_environment(config))
    else:
        command.extend(["--", *forwarded])
    return command, env


def run_one(config: dict[str, Any]) -> int:
    command, env = build_memory_ab_command(config)
    evaluation_root = Path(__file__).resolve().parent
    env["PYTHONPATH"] = str(evaluation_root)
    completed = subprocess.run(command, cwd=evaluation_root, env=env, check=False)
    return int(completed.returncode)


def _build_forwarded_args(config: dict[str, Any]) -> list[str]:
    dataset = str(config["dataset_name"])
    embedding = config.get("embedding", {})
    retrieval = config.get("retrieval", {})
    graph = config.get("graph", {})
    rerank = config.get("rerank", {})
    memory = config.get("memory", {})
    answer = config.get("answer", {})
    model_flag = "--embedding-model" if dataset == "longmemeval" else "--model"
    top_k_flag = "--retrieval-top-k" if dataset == "longmemeval" else "--top-k"
    args = [
        "--embedding",
        str(embedding.get("provider", "openrouter")),
        "--api-key-env",
        str(embedding.get("api_key_env", "OPENROUTER_API_KEY")),
        model_flag,
        str(embedding.get("model", "baai/bge-m3")),
        "--dimensions",
        str(embedding.get("dimensions", 1024)),
        top_k_flag,
        str(retrieval.get("top_k", 30)),
        "--max-graph-context-facts",
        str(graph.get("max_context_facts", 3)),
        "--extraction-model",
        str(memory.get("extraction_model", "openai/gpt-4o-mini")),
        "--verifier-model",
        str(memory.get("verifier_model", "openai/gpt-4o-mini")),
        "--extraction-api-key-env",
        str(memory.get("api_key_env", "OPENROUTER_API_KEY")),
        "--extraction-base-url",
        str(memory.get("base_url", "https://openrouter.ai/api/v1")),
        "--max-candidate-tokens",
        str(memory.get("max_candidate_tokens", 320)),
        "--max-window-tokens",
        str(memory.get("max_window_tokens", 640)),
        "--context-before-messages",
        str(memory.get("context_before_messages", 2)),
        "--context-after-messages",
        str(memory.get("context_after_messages", 0)),
    ]
    if dataset != "longmemeval":
        retrieval_args = [
            "--search-mode",
            str(retrieval.get("mode", "hybrid")),
            "--embedding-weight",
            str(retrieval.get("embedding_weight", 0.7)),
            "--bm25-weight",
            str(retrieval.get("bm25_weight", 0.3)),
            "--candidate-k",
            str(retrieval.get("candidate_k", 150)),
        ]
        args[args.index(top_k_flag):args.index(top_k_flag)] = retrieval_args
    if graph.get("build_enabled", False):
        args.extend(["--graph-build", "--graph-build-concurrency", str(graph.get("build_concurrency", 1))])
    if graph.get("enabled", False):
        args.extend([
            "--graph",
            "--graph-weight",
            str(graph.get("weight", 0.2)),
            "--graph-memory-space-mode",
            str(graph.get("memory_space_mode", "auto")),
            "--graph-memory-space-field",
            str(graph.get("memory_space_field", "scope_id")),
            "--graph-owner-id",
            str(graph.get("owner_id", "benchmark")),
            "--graph-llm-api-key-env",
            str(graph.get("llm_api_key_env", "OPENROUTER_API_KEY")),
            "--graph-llm-model",
            str(graph.get("llm_model", "openai/gpt-4o-mini")),
            "--graph-llm-base-url",
            str(graph.get("llm_base_url", "https://openrouter.ai/api/v1")),
            "--graph-llm-timeout-ms",
            str(graph.get("llm_timeout_ms", 60000)),
        ])
        if graph.get("rerank", False):
            args.append("--graph-rerank")
        if graph.get("allow_graph_only", False):
            args.append("--graph-allow-graph-only")
    if rerank.get("enabled", False):
        args.extend([
            "--rerank",
            "--rerank-provider",
            str(rerank.get("provider", "openrouter")),
            "--rerank-model",
            str(rerank.get("model", "cohere/rerank-v3.5")),
            "--rerank-api-key-env",
            str(rerank.get("api_key_env", "OPENROUTER_API_KEY")),
            "--rerank-base-url",
            str(rerank.get("base_url", "https://openrouter.ai/api/v1")),
            "--rerank-input-k",
            str(rerank.get("input_k", 40)),
        ])
    if answer.get("model"):
        answer_flag = "--answerer-model" if dataset == "longmemeval" else "--answer-model"
        args.extend([answer_flag, str(answer["model"])])
    if dataset == "longmemeval" and config.get("judge", {}).get("model"):
        args.extend(["--judge-model", str(config["judge"]["model"])])
    if dataset == "longmemeval" and answer.get("qa_top_k") is not None:
        args.extend(["--qa-top-k", str(answer["qa_top_k"])])
    return args


def _locomo_environment(config: dict[str, Any]) -> dict[str, str]:
    embedding = config.get("embedding", {})
    retrieval = config.get("retrieval", {})
    graph = config.get("graph", {})
    rerank = config.get("rerank", {})
    return {
        "MODEL": str(config.get("answer", {}).get("model", "openai/gpt-4o-mini")),
        "EMBEDDING_PROVIDER": str(embedding.get("provider", "openrouter")),
        "EMBEDDING_DIMENSIONS": str(embedding.get("dimensions", 1024)),
        "EMBEDDING_API_KEY_ENV": str(embedding.get("api_key_env", "OPENROUTER_API_KEY")),
        "OPENAI_BASE_URL": str(config.get("answer", {}).get("base_url", "https://openrouter.ai/api/v1")),
        "ANSWER_MAX_TOKENS": str(config.get("answer", {}).get("max_tokens", 512)),
        "MEMORY_BENCH_GRAPH": "1" if graph.get("enabled", False) else "0",
        "GRAPH_RERANK": "1" if graph.get("rerank", False) else "0",
        "GRAPH_ALLOW_GRAPH_ONLY": "1" if graph.get("allow_graph_only", False) else "0",
        "MAX_GRAPH_CONTEXT_FACTS": str(graph.get("max_context_facts", 3)),
        "GRAPH_BUILD_CONCURRENCY": str(graph.get("build_concurrency", 1)),
        "MEMORY_BENCH_SEARCH_MODE": str(retrieval.get("mode", "hybrid")),
        "GRAPH_WEIGHT": str(graph.get("weight", 0.2)),
        "GRAPH_MAX_GRAPH_ONLY_RESULTS": str(graph.get("max_graph_only_results", "")),
        "GRAPH_FAIL_OPEN": "1" if graph.get("fail_open", False) else "0",
        "GRAPH_MEMORY_SPACE_MODE": str(graph.get("memory_space_mode", "auto")),
        "GRAPH_MEMORY_SPACE_FIELD": str(graph.get("memory_space_field", "scope_id")),
        "GRAPH_OWNER_ID": str(graph.get("owner_id", "benchmark")),
        "GRAPH_LLM_API_KEY_ENV": str(graph.get("llm_api_key_env", "OPENROUTER_API_KEY")),
        "GRAPH_LLM_MODEL": str(graph.get("llm_model", "openai/gpt-4o-mini")),
        "GRAPH_LLM_BASE_URL": str(graph.get("llm_base_url", "https://openrouter.ai/api/v1")),
        "GRAPH_LLM_TIMEOUT_MS": str(graph.get("llm_timeout_ms", 60000)),
        "RERANK": "1" if rerank.get("enabled", False) else "0",
        "RERANK_PROVIDER": str(rerank.get("provider", "openrouter")),
        "RERANK_API_KEY_ENV": str(rerank.get("api_key_env", "OPENROUTER_API_KEY")),
        "RERANK_BASE_URL": str(rerank.get("base_url", "https://openrouter.ai/api/v1")),
        "RERANK_MODEL": str(rerank.get("model", "cohere/rerank-v3.5")),
        "RERANK_INPUT_K": str(rerank.get("input_k", 40)),
        "RERANK_TIMEOUT_MS": str(rerank.get("timeout_ms", "")),
        "RERANK_FAIL_OPEN": "1" if rerank.get("fail_open", False) else "0",
        "EMBEDDING_MODEL": str(embedding.get("model", "baai/bge-m3")),
        "TOP_K": str(retrieval.get("top_k", 30)),
        "CANDIDATE_K": str(retrieval.get("candidate_k", 150)),
        "EMBEDDING_WEIGHT": str(retrieval.get("embedding_weight", 0.7)),
        "BM25_WEIGHT": str(retrieval.get("bm25_weight", 0.3)),
    }


def _manifest_value(config: dict[str, Any]) -> dict[str, Any]:
    return _json_safe({
        key: value
        for key, value in config.items()
        if key not in {"config_path", "dataset_file", "manifest_path"}
    })


def _json_safe(value: Any) -> Any:
    if isinstance(value, Path):
        return str(value)
    if isinstance(value, dict):
        return {str(key): _json_safe(item) for key, item in value.items()}
    if isinstance(value, list):
        return [_json_safe(item) for item in value]
    return value


def _resolve_path(value: str, base: Path) -> Path:
    expanded = os.path.expandvars(os.path.expanduser(value))
    path = Path(expanded)
    return (path if path.is_absolute() else base / path).resolve()


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Run a RAM-A benchmark from one config file.")
    parser.add_argument("--config", required=True, type=Path)
    parser.add_argument("--dataset", choices=("locomo", "personalmem", "longmemeval"), required=True)
    return parser


def main() -> int:
    args = build_parser().parse_args()
    return run_one(load_runtime_config(args.config, args.dataset))


if __name__ == "__main__":
    raise SystemExit(main())
