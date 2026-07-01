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
) -> None:
    """Run ``memory-bench add`` to ingest memories from a prepared dataset."""
    cmd = CARGO_BIN + [
        "--store", str(store_path),
        "--embedding", embedding,
        "--model", model,
        "--dimensions", str(dimensions),
        "--api-key-env", api_key_env,
        "--batch-size", str(batch_size),
        "add",
        "--dataset", str(dataset_path),
    ]
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
) -> None:
    """Run ``memory-bench search`` to execute queries and save results."""
    cmd = CARGO_BIN + [
        "--store", str(store_path),
        "--embedding", embedding,
        "--model", model,
        "--dimensions", str(dimensions),
        "--api-key-env", api_key_env,
        "--batch-size", str(batch_size),
        "search",
        "--dataset", str(dataset_path),
        "--output", str(output_path),
        "--top-k", str(top_k),
    ]
    _run(cmd, f"search ({dataset_path.name})")


def _run(cmd: list[str], label: str) -> None:
    print(f"[runner] {label}")
    result = subprocess.run(cmd, capture_output=False, cwd=PROJECT_ROOT)
    if result.returncode != 0:
        raise MemoryBenchCommandError(f"memory-bench failed for {label} with exit code {result.returncode}")
    print(f"[runner] OK: {label}")
