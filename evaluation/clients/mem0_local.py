"""Small mem0 local SDK helper for evaluation adapters."""

from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path
from typing import Any


@dataclass
class Mem0LocalConfig:
    work_dir: Path
    collection_name: str
    embedding_model: str = "baai/bge-m3"
    embedding_dims: int = 1024
    llm_model: str = "gpt-4o-mini"
    api_key_env: str = "OPENAI_API_KEY"
    base_url: str | None = None
    on_disk: bool = False
    qdrant_path: Path | None = None
    history_db_path: Path | None = None

    def __post_init__(self) -> None:
        self.work_dir = Path(self.work_dir)
        if self.qdrant_path is None:
            self.qdrant_path = self.work_dir / "qdrant"
        else:
            self.qdrant_path = Path(self.qdrant_path)
        if self.history_db_path is None:
            self.history_db_path = self.work_dir / "history.db"
        else:
            self.history_db_path = Path(self.history_db_path)


def build_mem0_config(config: Mem0LocalConfig) -> dict[str, Any]:
    """Build the local Memory.from_config payload."""
    if config.api_key_env:
        api_key = os.getenv(config.api_key_env)
        if api_key:
            os.environ["OPENAI_API_KEY"] = api_key
    if config.base_url:
        os.environ["OPENAI_BASE_URL"] = config.base_url

    assert config.qdrant_path is not None
    assert config.history_db_path is not None
    config.qdrant_path.mkdir(parents=True, exist_ok=True)
    config.history_db_path.parent.mkdir(parents=True, exist_ok=True)

    return {
        "vector_store": {
            "provider": "qdrant",
            "config": {
                "path": str(config.qdrant_path),
                "collection_name": config.collection_name,
                "embedding_model_dims": config.embedding_dims,
                "on_disk": config.on_disk,
            },
        },
        "llm": {
            "provider": "openai",
            "config": {"model": config.llm_model},
        },
        "embedder": {
            "provider": "openai",
            "config": {"model": config.embedding_model},
        },
        "history_db_path": str(config.history_db_path),
    }


def create_memory(config: Mem0LocalConfig) -> Any:
    try:
        from mem0.memory.main import Memory
    except ModuleNotFoundError as error:
        raise RuntimeError(
            "mem0 local SDK is not installed. Install the PyPI package `mem0ai` "
            "in the active Python environment before running ingest/search."
        ) from error
    return Memory.from_config(build_mem0_config(config))


def close_memory(memory: Any) -> None:
    if hasattr(memory, "close"):
        memory.close()
    vector_store = getattr(memory, "vector_store", None)
    client = getattr(vector_store, "client", None)
    if hasattr(client, "close"):
        client.close()


def normalize_mem0_result(item: Any, scope_id: str) -> dict[str, Any]:
    if not isinstance(item, dict):
        item = {}

    metadata = item.get("metadata")
    if not isinstance(metadata, dict):
        metadata = {}
    metadata = dict(metadata)
    metadata.setdefault("scope_id", scope_id)

    text = item.get("memory")
    if text is None:
        text = item.get("text")
    if text is None:
        text = item.get("content")
    if text is None:
        text = ""

    score = item.get("score")
    try:
        score_value = float(score) if score is not None else 0.0
    except (TypeError, ValueError):
        score_value = 0.0

    return {
        "id": item.get("id") or item.get("memory_id") or item.get("uuid"),
        "text": str(text),
        "metadata": metadata,
        "score": score_value,
    }
