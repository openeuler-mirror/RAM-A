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
    graph_fail_open: bool = False,
    graph_memory_space_mode: str = "auto",
    graph_memory_space_field: str = "scope_id",
    graph_owner_id: str = "benchmark",
    graph_llm_api_key_env: str = DEFAULT_API_KEY_ENV,
    graph_llm_model: str = "openai/gpt-4o-mini",
    graph_llm_base_url: str = "https://openrouter.ai/api/v1",
    graph_llm_timeout_ms: int | None = None,
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


def _run(cmd: list[str], label: str) -> None:
    print(f"[runner] {label}")
    result = subprocess.run(cmd, capture_output=False, cwd=PROJECT_ROOT)
    if result.returncode != 0:
        raise MemoryBenchCommandError(f"memory-bench failed for {label} with exit code {result.returncode}")
    print(f"[runner] OK: {label}")
