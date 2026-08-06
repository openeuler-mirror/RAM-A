"""Shell runner for memory-bench CLI commands.

Wraps ``memory-bench add`` and ``memory-bench search`` subcommands that
operate on a single prepared-schema JSON dataset file.
"""

import subprocess
from pathlib import Path

from common.config import (
    CARGO_BIN,
    DEFAULT_API_KEY_ENV,
    DEFAULT_DIMENSIONS,
    DEFAULT_EMBEDDING_MODEL,
    PROJECT_ROOT,
)


class MemoryBenchCommandError(RuntimeError):
    """Raised when the memory-bench CLI exits unsuccessfully."""


def run_add(
    store_path: Path,
    dataset_path: Path,
    embedding: str = "hash",
    model: str = DEFAULT_EMBEDDING_MODEL,
    dimensions: int = DEFAULT_DIMENSIONS,
    api_key_env: str = DEFAULT_API_KEY_ENV,
    batch_size: int = 64,
    graph_build: bool = False,
    graph_build_concurrency: int = 1,
    resume: bool = False,
    graph_weight: float = 0.2,
    graph_memory_space_mode: str = "auto",
    graph_memory_space_field: str = "scope_id",
    graph_owner_id: str = "benchmark",
    graph_llm_api_key_env: str = DEFAULT_API_KEY_ENV,
    graph_llm_model: str = "openai/gpt-4o-mini",
    graph_llm_base_url: str = "https://openrouter.ai/api/v1",
    graph_llm_timeout_ms: int | None = None,
) -> None:
    """Run ``memory-bench add`` to ingest memories from a prepared dataset."""
    cmd = CARGO_BIN + [
        "--store", str(store_path),
        "--embedding", embedding,
        "--model", model,
        "--dimensions", str(dimensions),
        "--api-key-env", api_key_env,
        "--batch-size", str(batch_size),
    ]
    if graph_build:
        cmd.append("--graph-build")
        cmd.extend(["--graph-build-concurrency", str(graph_build_concurrency)])
        cmd.extend(_graph_common_args(
            graph_weight=graph_weight,
            graph_memory_space_mode=graph_memory_space_mode,
            graph_memory_space_field=graph_memory_space_field,
            graph_owner_id=graph_owner_id,
            graph_llm_api_key_env=graph_llm_api_key_env,
            graph_llm_model=graph_llm_model,
            graph_llm_base_url=graph_llm_base_url,
            graph_llm_timeout_ms=graph_llm_timeout_ms,
        ))
    cmd.extend(["add", "--dataset", str(dataset_path)])
    if resume:
        cmd.append("--resume")
    _run(cmd, f"add ({dataset_path.name})")


def run_search(
    store_path: Path,
    dataset_path: Path,
    output_path: Path,
    embedding: str = "hash",
    model: str = DEFAULT_EMBEDDING_MODEL,
    dimensions: int = DEFAULT_DIMENSIONS,
    api_key_env: str = DEFAULT_API_KEY_ENV,
    top_k: int = 10,
    batch_size: int = 64,
    graph: bool = False,
    graph_weight: float = 0.2,
    graph_rerank: bool = False,
    graph_allow_graph_only: bool = False,
    graph_max_graph_only_results: int | None = None,
    graph_fail_open: bool = False,
    graph_memory_space_mode: str = "auto",
    graph_memory_space_field: str = "scope_id",
    graph_owner_id: str = "benchmark",
    graph_llm_api_key_env: str = DEFAULT_API_KEY_ENV,
    graph_llm_model: str = "openai/gpt-4o-mini",
    graph_llm_base_url: str = "https://openrouter.ai/api/v1",
    graph_llm_timeout_ms: int | None = None,
    rerank: bool = False,
    rerank_provider: str = "openrouter",
    rerank_model: str = "cohere/rerank-v3.5",
    rerank_api_key_env: str = DEFAULT_API_KEY_ENV,
    rerank_base_url: str = "https://openrouter.ai/api/v1",
    rerank_input_k: int = 40,
    rerank_timeout_ms: int | None = None,
    rerank_fail_open: bool = False,
) -> None:
    """Run ``memory-bench search`` to execute queries and save results."""
    cmd = CARGO_BIN + [
        "--store", str(store_path),
        "--embedding", embedding,
        "--model", model,
        "--dimensions", str(dimensions),
        "--api-key-env", api_key_env,
        "--batch-size", str(batch_size),
    ]
    if graph:
        cmd.append("--graph")
        if graph_rerank:
            cmd.append("--graph-rerank")
        if graph_allow_graph_only:
            cmd.append("--graph-allow-graph-only")
        if graph_max_graph_only_results is not None:
            cmd.extend([
                "--graph-max-graph-only-results",
                str(graph_max_graph_only_results),
            ])
        if graph_fail_open:
            cmd.append("--graph-fail-open")
        cmd.extend(_graph_common_args(
            graph_weight=graph_weight,
            graph_memory_space_mode=graph_memory_space_mode,
            graph_memory_space_field=graph_memory_space_field,
            graph_owner_id=graph_owner_id,
            graph_llm_api_key_env=graph_llm_api_key_env,
            graph_llm_model=graph_llm_model,
            graph_llm_base_url=graph_llm_base_url,
            graph_llm_timeout_ms=graph_llm_timeout_ms,
        ))
    if rerank:
        cmd.extend(_rerank_args(
            rerank_provider=rerank_provider,
            rerank_model=rerank_model,
            rerank_api_key_env=rerank_api_key_env,
            rerank_base_url=rerank_base_url,
            rerank_input_k=rerank_input_k,
            rerank_timeout_ms=rerank_timeout_ms,
            rerank_fail_open=rerank_fail_open,
        ))
    cmd.extend([
        "search",
        "--dataset", str(dataset_path),
        "--output", str(output_path),
        "--top-k", str(top_k),
    ])
    _run(cmd, f"search ({dataset_path.name})")


def _graph_common_args(
    *,
    graph_weight: float,
    graph_memory_space_mode: str,
    graph_memory_space_field: str,
    graph_owner_id: str,
    graph_llm_api_key_env: str,
    graph_llm_model: str,
    graph_llm_base_url: str,
    graph_llm_timeout_ms: int | None,
) -> list[str]:
    args = [
        "--graph-weight", str(graph_weight),
        "--graph-memory-space-mode", graph_memory_space_mode,
        "--graph-memory-space-field", graph_memory_space_field,
        "--graph-owner-id", graph_owner_id,
        "--graph-llm-api-key-env", graph_llm_api_key_env,
        "--graph-llm-model", graph_llm_model,
        "--graph-llm-base-url", graph_llm_base_url,
    ]
    if graph_llm_timeout_ms is not None:
        args.extend(["--graph-llm-timeout-ms", str(graph_llm_timeout_ms)])
    return args


def _rerank_args(
    *,
    rerank_provider: str,
    rerank_model: str,
    rerank_api_key_env: str,
    rerank_base_url: str,
    rerank_input_k: int,
    rerank_timeout_ms: int | None,
    rerank_fail_open: bool,
) -> list[str]:
    args = [
        "--rerank",
        "--rerank-provider", rerank_provider,
        "--rerank-model", rerank_model,
        "--rerank-api-key-env", rerank_api_key_env,
        "--rerank-base-url", rerank_base_url,
        "--rerank-input-k", str(rerank_input_k),
    ]
    if rerank_timeout_ms is not None:
        args.extend(["--rerank-timeout-ms", str(rerank_timeout_ms)])
    if rerank_fail_open:
        args.append("--rerank-fail-open")
    return args


def validate_rerank_config(
    *,
    enabled: bool,
    provider: str,
    input_k: int,
    timeout_ms: int | None,
    search_mode: str = "hybrid",
) -> None:
    if not enabled:
        return
    if search_mode != "hybrid":
        raise ValueError("--rerank requires --search-mode hybrid")
    if provider != "openrouter":
        raise ValueError("--rerank-provider must be openrouter")
    if input_k < 0:
        raise ValueError("--rerank-input-k must be non-negative")
    if timeout_ms is not None and timeout_ms <= 0:
        raise ValueError("--rerank-timeout-ms must be greater than 0")


def validate_graph_search_config(
    *,
    graph: bool,
    rerank: bool,
    allow_graph_only: bool,
    max_graph_only_results: int | None,
) -> None:
    if rerank and not graph:
        raise ValueError("--graph-rerank requires --graph")
    if allow_graph_only and not rerank:
        raise ValueError("--graph-allow-graph-only requires --graph-rerank")
    if max_graph_only_results is not None and not allow_graph_only:
        raise ValueError(
            "--graph-max-graph-only-results requires --graph-allow-graph-only"
        )
    if max_graph_only_results is not None and max_graph_only_results < 0:
        raise ValueError("--graph-max-graph-only-results must be non-negative")


def _run(cmd: list[str], label: str) -> None:
    print(f"[runner] {label}")
    result = subprocess.run(cmd, capture_output=False, cwd=PROJECT_ROOT)
    if result.returncode != 0:
        raise MemoryBenchCommandError(f"memory-bench failed for {label} with exit code {result.returncode}")
    print(f"[runner] OK: {label}")
