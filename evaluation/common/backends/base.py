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
    graph: bool = False
    graph_build: bool = False
    graph_build_concurrency: int = 1
    resume: bool = False
    graph_weight: float = 0.2
    graph_rerank: bool = False
    graph_allow_graph_only: bool = False
    graph_max_graph_only_results: int | None = None
    graph_fail_open: bool = False
    graph_memory_space_mode: str = "auto"
    graph_memory_space_field: str = "scope_id"
    graph_owner_id: str = "benchmark"
    graph_llm_api_key_env: str = "OPENROUTER_API_KEY"
    graph_llm_model: str = "openai/gpt-4o-mini"
    graph_llm_base_url: str = "https://openrouter.ai/api/v1"
    graph_llm_timeout_ms: int | None = None
    rerank: bool = False
    rerank_provider: str = "openrouter"
    rerank_model: str = "cohere/rerank-v3.5"
    rerank_api_key_env: str = "OPENROUTER_API_KEY"
    rerank_base_url: str = "https://openrouter.ai/api/v1"
    rerank_input_k: int = 40
    rerank_timeout_ms: int | None = None
    rerank_fail_open: bool = False


class MemoryBackend:
    """Adapter from prepared benchmark JSON to a memory system."""

    name = "base"
    persists_local_store = False

    def add(self, prepared_path: Path) -> None:
        raise NotImplementedError

    def search(self, prepared_path: Path, output_path: Path) -> None:
        raise NotImplementedError
