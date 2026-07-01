"""Common backend interface for benchmark-prepared datasets."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class BackendConfig:
    name: str
    store_path: Path
    embedding: str
    embedding_model: str
    dimensions: int
    api_key_env: str
    batch_size: int
    top_k: int


class MemoryBackend:
    """Adapter from prepared benchmark JSON to a memory system."""

    name = "base"
    persists_local_store = False

    def add(self, prepared_path: Path) -> None:
        raise NotImplementedError

    def search(self, prepared_path: Path, output_path: Path) -> None:
        raise NotImplementedError
