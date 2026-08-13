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
rerank = true
allow_graph_only = true
max_graph_only_results = 4
max_context_facts = 3
llm_api_key_env = "GRAPH_KEY"
llm_model = "graph/model"
llm_base_url = "https://graph.example/v1"
llm_timeout_ms = 12000

[rerank]
enabled = true
provider = "openrouter"
api_key_env = "RERANK_KEY"
model = "cohere/rerank-v3.5"
base_url = "https://rerank.example/v1"
input_k = 40
timeout_ms = 15000
fail_open = true

[answer]
model = "answer/model"
api_key_env = "CHAT_KEY"
base_url = "https://chat.example/v1"
qa_top_k = 10

[judge]
model = "judge/model"
api_key_env = "CHAT_KEY"
base_url = "https://chat.example/v1"
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
    assert env["MEMORY_BENCH_GRAPH_BUILD"] == "1"
    assert env["RERANK"] == "1"
    assert env["GRAPH_WEIGHT"] == "0.2"
    assert env["GRAPH_LLM_MODEL"] == "openai/gpt-4o-mini"
    assert env["MODEL"] == "openai/gpt-4o-mini"
    assert env["OPENAI_BASE_URL"] == "https://openrouter.ai/api/v1"
    assert env["RERANK_MODEL"] == "cohere/rerank-v3.5"
    assert env["RERANK_INPUT_K"] == "40"
    assert env["MEMORY_EXTRACTION_MODEL"] == "openai/gpt-4o-mini"
    assert env["MEMORY_VERIFIER_MODEL"] == "openai/gpt-4o-mini"
    assert env["ANSWER_MODEL"] == "openai/gpt-4o-mini"
    assert env["ANSWER_API_KEY_ENV"] == "OPENROUTER_API_KEY"
    assert env["JUDGE_MODEL"] == "openai/gpt-4o-mini"
    assert env["JUDGE_API_KEY_ENV"] == "OPENROUTER_API_KEY"
    assert json.loads((runtime["manifest_path"]).read_text(encoding="utf-8"))["mode"] == "normal"


def test_config_runner_maps_custom_locomo_provider_and_graph_values(
    tmp_path: Path,
) -> None:
    dataset = tmp_path / "locomo10.json"
    dataset.write_text("{}\n", encoding="utf-8")
    runtime = load_runtime_config(_write_config(tmp_path, "locomo", dataset), "locomo")

    _, env = build_memory_ab_command(runtime)

    assert env["EMBEDDING_MODEL"] == "baai/bge-m3"
    assert env["MEMORY_BENCH_SEARCH_MODE"] == "hybrid"
    assert env["CANDIDATE_K"] == "150"
    assert env["MEMORY_BENCH_GRAPH"] == "1"
    assert env["MEMORY_BENCH_GRAPH_BUILD"] == "1"
    assert env["GRAPH_RERANK"] == "1"
    assert env["GRAPH_ALLOW_GRAPH_ONLY"] == "1"
    assert env["GRAPH_MAX_GRAPH_ONLY_RESULTS"] == "4"
    assert env["GRAPH_LLM_API_KEY_ENV"] == "GRAPH_KEY"
    assert env["GRAPH_LLM_BASE_URL"] == "https://graph.example/v1"
    assert env["RERANK_API_KEY_ENV"] == "RERANK_KEY"
    assert env["RERANK_BASE_URL"] == "https://rerank.example/v1"
    assert env["RERANK_TIMEOUT_MS"] == "15000"
    assert env["RERANK_FAIL_OPEN"] == "1"
    assert env["ANSWER_API_KEY_ENV"] == "CHAT_KEY"
    assert env["ANSWER_BASE_URL"] == "https://chat.example/v1"
    assert env["JUDGE_API_KEY_ENV"] == "CHAT_KEY"
    assert env["JUDGE_BASE_URL"] == "https://chat.example/v1"


def test_config_runner_does_not_inject_missing_embedding_api_key(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    dataset = tmp_path / "locomo10.json"
    dataset.write_text("{}\n", encoding="utf-8")
    monkeypatch.delenv("OPENROUTER_API_KEY", raising=False)
    runtime = load_runtime_config(
        _write_config(tmp_path, "locomo", dataset),
        "locomo",
    )

    _, env = build_memory_ab_command(runtime)

    assert "OPENROUTER_API_KEY" not in env


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
    assert forwarded[forwarded.index("--search-mode") + 1] == "hybrid"
    assert forwarded[forwarded.index("--embedding-weight") + 1] == "0.7"
    assert forwarded[forwarded.index("--bm25-weight") + 1] == "0.3"
    assert forwarded[forwarded.index("--candidate-k") + 1] == "150"
    assert forwarded[forwarded.index("--rerank-timeout-ms") + 1] == "15000"
    assert "--rerank-fail-open" in forwarded
    assert forwarded[forwarded.index("--graph-max-graph-only-results") + 1] == "4"
    assert forwarded[forwarded.index("--llm-api-key-env") + 1] == "CHAT_KEY"
    assert forwarded[forwarded.index("--llm-base-url") + 1] == "https://chat.example/v1"
    assert command.count("--embedding") == 1


def test_config_runner_forwards_personalmem_embedding_key_flag_once(tmp_path: Path) -> None:
    dataset = tmp_path / "personalmem.json"
    dataset.write_text("{}\n", encoding="utf-8")
    runtime = load_runtime_config(_write_config(tmp_path, "personalmem", dataset), "personalmem")

    command, _ = build_memory_ab_command(runtime)
    forwarded = command[command.index("--") + 1 :]

    assert command.count("--embedding") == 1
    assert forwarded.count("--api-key-env") == 1
    assert forwarded[forwarded.index("--answer-api-key-env") + 1] == "CHAT_KEY"
    assert forwarded[forwarded.index("--answer-base-url") + 1] == "https://chat.example/v1"
    assert forwarded[forwarded.index("--rerank-timeout-ms") + 1] == "15000"
    assert "--rerank-fail-open" in forwarded


def test_config_runner_forwards_graph_provider_when_only_build_is_enabled(
    tmp_path: Path,
) -> None:
    dataset = tmp_path / "personalmem.json"
    dataset.write_text("{}\n", encoding="utf-8")
    config = _write_config(tmp_path, "personalmem", dataset)
    config.write_text(
        config.read_text(encoding="utf-8").replace(
            "[graph]\nenabled = true",
            "[graph]\nenabled = false",
        ),
        encoding="utf-8",
    )

    runtime = load_runtime_config(config, "personalmem")
    command, _ = build_memory_ab_command(runtime)
    forwarded = command[command.index("--") + 1 :]

    assert "--graph-build" in forwarded
    assert "--graph" not in forwarded
    assert forwarded[forwarded.index("--graph-llm-api-key-env") + 1] == "GRAPH_KEY"
    assert forwarded[forwarded.index("--graph-llm-model") + 1] == "graph/model"
    assert forwarded[forwarded.index("--graph-llm-base-url") + 1] == "https://graph.example/v1"
    assert forwarded[forwarded.index("--graph-llm-timeout-ms") + 1] == "12000"


def test_runtime_config_rejects_unsafe_pair_id_before_writing_manifest(
    tmp_path: Path,
) -> None:
    dataset = tmp_path / "locomo.json"
    dataset.write_text("{}\n", encoding="utf-8")
    config = _write_config(tmp_path, "locomo", dataset)
    config.write_text(
        config.read_text(encoding="utf-8").replace(
            'pair_id = "graph-full-v1"',
            'pair_id = "../../outside"',
        ),
        encoding="utf-8",
    )

    with pytest.raises(ValueError):
        load_runtime_config(config, "locomo")

    assert not (tmp_path / "outside" / "run_manifest.json").exists()


def test_config_hash_is_stable_across_machine_specific_paths(tmp_path: Path) -> None:
    manifests = []
    for name in ("checkout-a", "checkout-b"):
        root = tmp_path / name
        root.mkdir()
        dataset = root / "locomo.json"
        dataset.write_text("{}\n", encoding="utf-8")
        policy = root / "policy.json"
        policy.write_text(
            '{"schema_version":"memory-ab-promotion-v1"}\n',
            encoding="utf-8",
        )
        config = _write_config(root, "locomo", dataset)
        config.write_text(
            config.read_text(encoding="utf-8")
            .replace('mode = "normal"', 'mode = "strict"')
            .replace(
                'execution = "ab"',
                f'execution = "ab"\npromotion_policy = "{policy}"',
            ),
            encoding="utf-8",
        )
        runtime = load_runtime_config(config, "locomo")
        manifests.append(
            json.loads(runtime["manifest_path"].read_text(encoding="utf-8"))
        )

    assert manifests[0]["dataset_hash"] == manifests[1]["dataset_hash"]
    assert manifests[0]["promotion_policy_hash"] == manifests[1]["promotion_policy_hash"]
    assert manifests[0]["config_hash"] == manifests[1]["config_hash"]


def test_longmemeval_rejects_separate_answer_and_judge_providers(
    tmp_path: Path,
) -> None:
    dataset = tmp_path / "longmemeval.json"
    dataset.write_text("{}\n", encoding="utf-8")
    config = _write_config(tmp_path, "longmemeval", dataset)
    config.write_text(
        config.read_text(encoding="utf-8").replace(
            '[judge]\nmodel = "judge/model"\napi_key_env = "CHAT_KEY"',
            '[judge]\nmodel = "judge/model"\napi_key_env = "OTHER_CHAT_KEY"',
        ),
        encoding="utf-8",
    )

    with pytest.raises(ValueError, match="LongMemEval answer and judge"):
        load_runtime_config(config, "longmemeval")


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


def test_config_runner_forwards_resume(tmp_path: Path) -> None:
    dataset = tmp_path / "personalmem.json"
    dataset.write_text("{}\n", encoding="utf-8")
    config = _write_config(tmp_path, "personalmem", dataset)
    config.write_text(
        config.read_text(encoding="utf-8").replace(
            'execution = "ab"',
            'execution = "ab"\nresume = true',
        ),
        encoding="utf-8",
    )

    runtime = load_runtime_config(config, "personalmem")
    command, _ = build_memory_ab_command(runtime)

    assert command.count("--resume") == 1
