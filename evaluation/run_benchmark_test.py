from __future__ import annotations

import json
from pathlib import Path
import sys

import pytest

from run_benchmark import build_memory_ab_command, load_runtime_config


def _write_config(tmp_path: Path, dataset: str, dataset_file: Path) -> Path:
    config = tmp_path / f"{dataset}.toml"
    config.write_text(
        f"""
[run]
phase = "full"
mode = "normal"
execution = "ab"
pair_id = "graph-full-v1"
output_root = "{tmp_path / 'outputs'}"

[dataset.{dataset}]
file = "{dataset_file}"

[embedding]
provider = "openrouter"
api_key_env = "OPENROUTER_API_KEY"
model = "baai/bge-m3"
dimensions = 1024

[retrieval]
mode = "hybrid"
embedding_weight = 0.7
bm25_weight = 0.3
candidate_k = 150
top_k = 30

[graph]
enabled = true
build_enabled = true
weight = 0.2
max_context_facts = 3

[rerank]
enabled = true
model = "cohere/rerank-v3.5"
input_k = 40

[answer]
qa_top_k = 10
""",
        encoding="utf-8",
    )
    return config


def test_config_runner_builds_short_normal_command(tmp_path: Path, monkeypatch) -> None:
    dataset = tmp_path / "locomo10.json"
    dataset.write_text("{}\n", encoding="utf-8")
    config = tmp_path / "benchmark.toml"
    config.write_text(
        f"""
[run]
phase = "full"
mode = "normal"
pair_id = "graph-full-v1"
output_root = "{tmp_path / 'outputs'}"

[dataset.locomo]
file = "{dataset}"

[embedding]
provider = "openrouter"
api_key_env = "OPENROUTER_API_KEY"
model = "baai/bge-m3"
dimensions = 1024

[retrieval]
mode = "hybrid"
embedding_weight = 0.7
bm25_weight = 0.3
candidate_k = 150
top_k = 30

[graph]
enabled = true
build_enabled = true
weight = 0.2
max_context_facts = 3

[rerank]
enabled = true
model = "cohere/rerank-v3.5"
input_k = 40
""",
        encoding="utf-8",
    )
    monkeypatch.setenv("OPENROUTER_API_KEY", "test-key")

    runtime = load_runtime_config(config, "locomo")
    command, env = build_memory_ab_command(runtime)

    assert command[:3] == [sys.executable, "-m", "scripts.run_memory_ab"]
    assert "--mode" in command and command[command.index("--mode") + 1] == "normal"
    assert "--frozen-config" not in command
    assert env["MEMORY_BENCH_GRAPH"] == "1"
    assert env["RERANK"] == "1"
    assert env["GRAPH_WEIGHT"] == "0.2"
    assert env["GRAPH_LLM_MODEL"] == "openai/gpt-4o-mini"
    assert env["MODEL"] == "openai/gpt-4o-mini"
    assert env["OPENAI_BASE_URL"] == "https://openrouter.ai/api/v1"
    assert env["RERANK_MODEL"] == "cohere/rerank-v3.5"
    assert env["RERANK_INPUT_K"] == "40"
    assert json.loads((runtime["manifest_path"]).read_text(encoding="utf-8"))["mode"] == "normal"


def test_normal_config_rejects_promotion_policy(tmp_path: Path) -> None:
    dataset = tmp_path / "locomo10.json"
    dataset.write_text("{}\n", encoding="utf-8")
    config = _write_config(tmp_path, "locomo", dataset)
    config.write_text(
        config.read_text(encoding="utf-8").replace(
            'mode = "normal"',
            f'mode = "normal"\npromotion_policy = "{tmp_path / "policy.json"}"',
        ),
        encoding="utf-8",
    )
    (tmp_path / "policy.json").write_text("{}\n", encoding="utf-8")

    with pytest.raises(ValueError, match="only valid in strict mode"):
        load_runtime_config(config, "locomo")


def test_config_runner_maps_longmemeval_specific_embedding_and_top_k_flags(tmp_path: Path) -> None:
    dataset = tmp_path / "longmemeval.json"
    dataset.write_text("{}\n", encoding="utf-8")
    runtime = load_runtime_config(_write_config(tmp_path, "longmemeval", dataset), "longmemeval")

    command, _ = build_memory_ab_command(runtime)
    forwarded = command[command.index("--") + 1 :]

    assert forwarded[forwarded.index("--embedding-model") + 1] == "baai/bge-m3"
    assert forwarded[forwarded.index("--retrieval-top-k") + 1] == "30"
    assert forwarded[forwarded.index("--qa-top-k") + 1] == "10"
    assert "--model" not in forwarded
    assert "--top-k" not in forwarded
    assert "--search-mode" not in forwarded
    assert "--candidate-k" not in forwarded
    assert command.count("--embedding") == 1


def test_config_runner_forwards_personalmem_embedding_key_flag_once(tmp_path: Path) -> None:
    dataset = tmp_path / "personalmem.json"
    dataset.write_text("{}\n", encoding="utf-8")
    runtime = load_runtime_config(_write_config(tmp_path, "personalmem", dataset), "personalmem")

    command, _ = build_memory_ab_command(runtime)
    forwarded = command[command.index("--") + 1 :]

    assert command.count("--embedding") == 1
    assert forwarded.count("--api-key-env") == 1


def test_config_runner_builds_single_extracted_command(tmp_path: Path) -> None:
    dataset = tmp_path / "locomo.json"
    dataset.write_text("{}\n", encoding="utf-8")
    config = _write_config(tmp_path, "locomo", dataset)
    text = config.read_text(encoding="utf-8").replace(
        'execution = "ab"', 'execution = "single"\nmemory_mode = "extracted"'
    )
    config.write_text(text, encoding="utf-8")

    runtime = load_runtime_config(config, "locomo")
    command, _ = build_memory_ab_command(runtime)

    assert "--execution" in command
    assert command[command.index("--execution") + 1] == "single"
    assert command[command.index("--memory-mode") + 1] == "extracted"
