"""RAM-A backend implemented by memory-bench.

The public backend key is `RAM-A`.
"""

from __future__ import annotations

from pathlib import Path

from common.runner import run_add, run_search

from .base import BackendConfig, MemoryBackend


class RamaBackend(MemoryBackend):
    name = "RAM-A"
    persists_local_store = True

    def __init__(self, config: BackendConfig):
        self.config = config

    def add(self, prepared_path: Path) -> None:
        run_add(
            self.config.store_path,
            prepared_path,
            embedding=self.config.embedding,
            model=self.config.embedding_model,
            dimensions=self.config.dimensions,
            api_key_env=self.config.api_key_env,
            batch_size=self.config.batch_size,
        )

    def search(self, prepared_path: Path, output_path: Path) -> None:
        run_search(
            self.config.store_path,
            prepared_path,
            output_path,
            embedding=self.config.embedding,
            model=self.config.embedding_model,
            dimensions=self.config.dimensions,
            api_key_env=self.config.api_key_env,
            top_k=self.config.top_k,
            batch_size=self.config.batch_size,
        )
