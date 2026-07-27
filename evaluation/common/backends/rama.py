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
            graph_build=self.config.graph_build,
            graph_build_concurrency=self.config.graph_build_concurrency,
            resume=self.config.resume,
            graph_weight=self.config.graph_weight,
            graph_memory_space_mode=self.config.graph_memory_space_mode,
            graph_memory_space_field=self.config.graph_memory_space_field,
            graph_owner_id=self.config.graph_owner_id,
            graph_llm_api_key_env=self.config.graph_llm_api_key_env,
            graph_llm_model=self.config.graph_llm_model,
            graph_llm_base_url=self.config.graph_llm_base_url,
            graph_llm_timeout_ms=self.config.graph_llm_timeout_ms,
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
            graph=self.config.graph,
            graph_weight=self.config.graph_weight,
            graph_fail_open=self.config.graph_fail_open,
            graph_memory_space_mode=self.config.graph_memory_space_mode,
            graph_memory_space_field=self.config.graph_memory_space_field,
            graph_owner_id=self.config.graph_owner_id,
            graph_llm_api_key_env=self.config.graph_llm_api_key_env,
            graph_llm_model=self.config.graph_llm_model,
            graph_llm_base_url=self.config.graph_llm_base_url,
            graph_llm_timeout_ms=self.config.graph_llm_timeout_ms,
        )
