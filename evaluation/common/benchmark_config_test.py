from __future__ import annotations

from pathlib import Path

import pytest

from common.benchmark_config import load_benchmark_config


def test_load_benchmark_config_expands_dataset_paths_and_defaults(tmp_path: Path, monkeypatch) -> None:
    dataset = tmp_path / "locomo10.json"
    dataset.write_text("{}\n", encoding="utf-8")
    config = tmp_path / "benchmark.toml"
    config.write_text(
        """
[run]
phase = "full"
mode = "normal"
pair_id = "graph-full-v1"

[dataset.locomo]
file = "${LOCOMO_DATASET}"

[retrieval]
top_k = 30

[graph]
enabled = true
build_enabled = true
weight = 0.2
""",
        encoding="utf-8",
    )
    monkeypatch.setenv("LOCOMO_DATASET", str(dataset))

    loaded = load_benchmark_config(config, "locomo")

    assert loaded["run"]["mode"] == "normal"
    assert loaded["dataset_file"] == dataset.resolve()
    assert loaded["retrieval"]["top_k"] == 30
    assert loaded["graph"]["enabled"] is True


def test_load_benchmark_config_rejects_missing_dataset_section(tmp_path: Path) -> None:
    config = tmp_path / "benchmark.toml"
    config.write_text('[run]\nphase = "full"\n', encoding="utf-8")

    with pytest.raises(ValueError, match="dataset.locomo"):
        load_benchmark_config(config, "locomo")
