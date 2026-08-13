"""Backend factory."""

from __future__ import annotations

from .base import BackendConfig, MemoryBackend
from .rama import RamaBackend


def create_backend(config: BackendConfig) -> MemoryBackend:
    if config.name == "RAM-A":
        return RamaBackend(config)
    raise ValueError(f"unknown backend: {config.name}. Supported backends: RAM-A")
