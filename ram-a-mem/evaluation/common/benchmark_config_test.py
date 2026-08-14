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
    assert loaded["run"]["resume"] is False
    assert loaded["dataset_file"] == dataset.resolve()
    assert loaded["retrieval"]["top_k"] == 30
    assert loaded["graph"]["enabled"] is True


def test_load_benchmark_config_rejects_missing_dataset_section(tmp_path: Path) -> None:
    config = tmp_path / "benchmark.toml"
    config.write_text('[run]\nphase = "full"\n', encoding="utf-8")

    with pytest.raises(ValueError, match="dataset.locomo"):
        load_benchmark_config(config, "locomo")


def test_load_benchmark_config_rejects_single_without_memory_mode(
    tmp_path: Path,
) -> None:
    dataset = tmp_path / "locomo10.json"
    dataset.write_text("{}\n", encoding="utf-8")
    config = tmp_path / "benchmark.toml"
    config.write_text(
        f"""
[run]
execution = "single"

[dataset.locomo]
file = "{dataset}"
""",
        encoding="utf-8",
    )

    with pytest.raises(
        ValueError,
        match="run.memory_mode must be raw or extracted for single runs",
    ):
        load_benchmark_config(config, "locomo")


def test_load_benchmark_config_rejects_strict_single_run(tmp_path: Path) -> None:
    dataset = tmp_path / "locomo10.json"
    dataset.write_text("{}\n", encoding="utf-8")
    config = tmp_path / "benchmark.toml"
    config.write_text(
        f"""
[run]
mode = "strict"
execution = "single"
memory_mode = "raw"

[dataset.locomo]
file = "{dataset}"
""",
        encoding="utf-8",
    )

    with pytest.raises(ValueError, match="single runs only support normal mode"):
        load_benchmark_config(config, "locomo")


def test_load_benchmark_config_rejects_memory_mode_for_ab_run(
    tmp_path: Path,
) -> None:
    dataset = tmp_path / "locomo10.json"
    dataset.write_text("{}\n", encoding="utf-8")
    config = tmp_path / "benchmark.toml"
    config.write_text(
        f"""
[run]
execution = "ab"
memory_mode = "raw"

[dataset.locomo]
file = "{dataset}"
""",
        encoding="utf-8",
    )

    with pytest.raises(ValueError, match="run.memory_mode is only valid for single runs"):
        load_benchmark_config(config, "locomo")


def test_load_benchmark_config_rejects_unknown_top_level_section(
    tmp_path: Path,
) -> None:
    dataset = tmp_path / "locomo10.json"
    dataset.write_text("{}\n", encoding="utf-8")
    config = tmp_path / "benchmark.toml"
    config.write_text(
        f"""
[dataset.locomo]
file = "{dataset}"

[retreival]
top_k = 30
""",
        encoding="utf-8",
    )

    with pytest.raises(ValueError, match=r"unknown benchmark config section: retreival"):
        load_benchmark_config(config, "locomo")


def test_load_benchmark_config_rejects_unknown_section_key(tmp_path: Path) -> None:
    dataset = tmp_path / "locomo10.json"
    dataset.write_text("{}\n", encoding="utf-8")
    config = tmp_path / "benchmark.toml"
    config.write_text(
        f"""
[dataset.locomo]
file = "{dataset}"

[graph]
enabled = true
allow_graph_ony = true
""",
        encoding="utf-8",
    )

    with pytest.raises(ValueError, match=r"unknown graph config key: allow_graph_ony"):
        load_benchmark_config(config, "locomo")


def test_load_benchmark_config_rejects_non_boolean_resume(tmp_path: Path) -> None:
    dataset = tmp_path / "locomo10.json"
    dataset.write_text("{}\n", encoding="utf-8")
    config = tmp_path / "benchmark.toml"
    config.write_text(
        f"""
[run]
resume = "false"

[dataset.locomo]
file = "{dataset}"
""",
        encoding="utf-8",
    )

    with pytest.raises(ValueError, match="run.resume must be a boolean"):
        load_benchmark_config(config, "locomo")


@pytest.mark.parametrize(
    ("section", "field"),
    (
        ("graph", "enabled"),
        ("graph", "build_enabled"),
        ("graph", "rerank"),
        ("graph", "allow_graph_only"),
        ("graph", "fail_open"),
        ("rerank", "enabled"),
        ("rerank", "fail_open"),
    ),
)
def test_load_benchmark_config_rejects_quoted_boolean_flags(
    tmp_path: Path,
    section: str,
    field: str,
) -> None:
    dataset = tmp_path / "locomo10.json"
    dataset.write_text("{}\n", encoding="utf-8")
    config = tmp_path / "benchmark.toml"
    config.write_text(
        f"""
[dataset.locomo]
file = "{dataset}"

[{section}]
{field} = "false"
""",
        encoding="utf-8",
    )

    with pytest.raises(ValueError, match=rf"{section}.{field} must be a boolean"):
        load_benchmark_config(config, "locomo")
