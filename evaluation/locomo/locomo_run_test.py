from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys

import pytest

import locomo.locomo_run as locomo_run

from locomo.locomo_adapter import prepare_locomo
from locomo.locomo_run import (
    RunConfig,
    build_add_command,
    build_extraction_command,
    build_search_command,
    config_hash,
    ensure_run_mode,
    memory_bench_base_command,
    run_arm,
    run_stage,
    stage_manifest,
)


FIXTURE = Path(__file__).parents[1] / "fixtures" / "locomo_sample.json"
EXTRACTOR_FIXTURE = (
    Path(__file__).parents[1] / "fixtures" / "locomo_memory_extractor_responses.json"
)
GROUNDING_FIXTURE = (
    Path(__file__).parents[1] / "fixtures" / "locomo_memory_grounding_responses.json"
)


def test_run_config_matches_approved_settings_without_serializing_secret(
    monkeypatch,
    tmp_path,
) -> None:
    monkeypatch.setenv("OPENROUTER_API_KEY", "SECRET_CANARY")
    config = RunConfig.from_env(
        {
            "MEMORY_MODE": "extracted",
            "PHASE": "full",
            "DATASET": str(tmp_path / "locomo.json"),
            "RUN_DIR": str(tmp_path / "run"),
        }
    )

    assert config.embedding_model == "baai/bge-m3"
    assert config.embedding_dimensions == 1024
    assert (config.embedding_weight, config.bm25_weight, config.candidate_k) == (
        0.7,
        0.3,
        150,
    )
    assert (config.rerank_model, config.rerank_input_k, config.top_k) == (
        "cohere/rerank-v3.5",
        40,
        30,
    )
    assert config.answer_max_tokens == 512
    assert config.max_graph_context_facts == 3
    serialized = json.dumps(config.public_manifest())
    assert "SECRET_CANARY" not in serialized
    assert config.public_manifest()["credential_env"] == "OPENROUTER_API_KEY"
    assert config.public_manifest()["prompt_versions"] == {
        "extraction": "extract_v2",
        "grounding": "ground_v1",
        "answer": "locomo_answer_v1",
        "judge": "locomo_accuracy_v1",
    }
    assert config.public_manifest()["extraction_schema_version"] == "atomic_memory_v1"
    assert config.public_manifest()["llm_temperature"] == 0.0
    assert len(config.immutable_manifest()["implementation_hash"]) == 64
    assert len(config_hash(config)) == 64


def test_run_config_records_pair_and_explicit_policy_hash(monkeypatch, tmp_path):
    policy = tmp_path / "policy.json"
    policy.write_text('{"schema_version":"locomo-promotion-v1"}\n', encoding="utf-8")

    config = RunConfig.from_env(
        {
            "PAIR_ID": "locomo-pair-7",
            "PROMOTION_POLICY": str(policy),
            "DATASET": str(tmp_path / "locomo.json"),
            "RUN_DIR": str(tmp_path / "run"),
        }
    )

    assert config.public_manifest()["pair_id"] == "locomo-pair-7"
    assert config.public_manifest()["promotion_policy_hash"] == locomo_run.file_sha256(policy)
    assert "pair_id" not in config.immutable_manifest()
    assert config.immutable_manifest()["promotion_policy_hash"] == locomo_run.file_sha256(policy)


def test_run_config_and_search_command_follow_runtime_config(monkeypatch, tmp_path):
    config = RunConfig.from_env(
        {
            "DATASET": str(tmp_path / "locomo.json"),
            "RUN_DIR": str(tmp_path / "run"),
            "MODEL": "answer-model",
            "EMBEDDING_MODEL": "embedding-model",
            "EMBEDDING_DIMENSIONS": "768",
            "EMBEDDING_WEIGHT": "0.6",
            "BM25_WEIGHT": "0.4",
            "CANDIDATE_K": "120",
            "TOP_K": "20",
            "RERANK": "0",
            "RERANK_PROVIDER": "openrouter",
            "RERANK_API_KEY_ENV": "RERANK_KEY",
            "RERANK_BASE_URL": "https://rerank.example/v1",
            "RERANK_MODEL": "rerank-model",
            "RERANK_INPUT_K": "25",
        }
    )

    assert config.chat_model == "answer-model"
    assert config.embedding_model == "embedding-model"
    assert config.embedding_dimensions == 768
    assert (config.embedding_weight, config.bm25_weight) == (0.6, 0.4)
    assert (config.candidate_k, config.top_k) == (120, 20)
    assert config.rerank_enabled is False
    command = build_search_command(
        config,
        tmp_path / "store.sqlite",
        tmp_path / "prepared.json",
        tmp_path / "results.json",
    )
    assert "--rerank" not in command
    assert command[command.index("--embedding") + 1] == "openrouter"
    assert command[command.index("--model") + 1] == "embedding-model"
    assert command[command.index("--dimensions") + 1] == "768"
    assert command[command.index("--candidate-k") + 1] == "120"
    assert command[command.index("--top-k") + 1] == "20"


def test_graph_context_limit_is_answer_only_configuration(tmp_path):
    common = {
        "memory_mode": "raw",
        "phase": "full",
        "dataset": tmp_path / "locomo.json",
        "run_dir": tmp_path / "run",
    }
    control = RunConfig(**common, max_graph_context_facts=0)
    treatment = RunConfig(**common, max_graph_context_facts=3)

    assert control.immutable_manifest() == treatment.immutable_manifest()
    assert control.public_manifest()["max_graph_context_facts"] == 0
    assert treatment.public_manifest()["max_graph_context_facts"] == 3


def test_direct_script_launcher_can_import_shared_contracts() -> None:
    env = dict(os.environ)
    env.pop("PYTHONPATH", None)

    result = subprocess.run(
        [sys.executable, "locomo/locomo_run.py", "--help"],
        cwd=locomo_run.EVALUATION_ROOT,
        env=env,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr


def test_implementation_hash_covers_shared_llm_client(monkeypatch, tmp_path) -> None:
    evaluation_root = tmp_path / "evaluation"
    project_root = tmp_path
    shared_client = evaluation_root / "common" / "llm_client.py"
    locomo_module = evaluation_root / "locomo" / "adapter.py"
    shared_client.parent.mkdir(parents=True)
    locomo_module.parent.mkdir(parents=True)
    shared_client.write_text("RETRIES = 3\n", encoding="utf-8")
    locomo_module.write_text("VERSION = 1\n", encoding="utf-8")
    monkeypatch.setattr(locomo_run, "EVALUATION_ROOT", evaluation_root)
    monkeypatch.setattr(locomo_run, "PROJECT_ROOT", project_root)

    before = locomo_run.implementation_hash()
    shared_client.write_text("RETRIES = 8\n", encoding="utf-8")
    after = locomo_run.implementation_hash()

    assert before != after


def test_memory_bench_command_contains_configured_retrieval_settings(tmp_path) -> None:
    config = RunConfig(
        memory_mode="raw",
        phase="full",
        dataset=tmp_path / "locomo.json",
        run_dir=tmp_path / "run",
    )

    command = memory_bench_base_command(config, tmp_path / "store.sqlite")

    assert command[-20:-2] == [
        "--store",
        str(tmp_path / "store.sqlite"),
        "--store-backend",
        "sqlite",
        "--embedding",
        "openrouter",
        "--api-key-env",
        "OPENROUTER_API_KEY",
        "--model",
        "baai/bge-m3",
        "--dimensions",
        "1024",
        "--search-mode",
        "hybrid",
        "--embedding-weight",
        "0.7",
        "--bm25-weight",
        "0.3",
    ]
    assert command[-2:] == ["--candidate-k", "150"]


def test_extraction_command_disables_fail_fast(tmp_path) -> None:
    config = RunConfig(
        memory_mode="extracted",
        phase="full",
        dataset=tmp_path / "locomo.json",
        run_dir=tmp_path / "run",
    )
    raw_prepared = tmp_path / "run" / "raw_prepared.json"
    indexed_prepared = tmp_path / "run" / "extracted_prepared.json"
    artifacts = tmp_path / "run" / "artifacts"

    command = build_extraction_command(
        config, raw_prepared, indexed_prepared, artifacts, "config-digest-abc"
    )

    assert "--no-fail-fast" in command
    assert command[:8] == [
        "cargo",
        "run",
        "--quiet",
        "--manifest-path",
        str(locomo_run.PROJECT_ROOT / "Cargo.toml"),
        "-p",
        "memory-pipeline",
        "--",
    ]
    assert "--model" in command
    assert command[command.index("--model") + 1] == "openai/gpt-4o-mini"
    assert command[command.index("--verifier-model") + 1] == "openai/gpt-4o-mini"
    assert command[command.index("--api-key-env") + 1] == "OPENROUTER_API_KEY"
    assert command[command.index("--cache-version") + 1] == "config-digest-abc"
    assert command[command.index("--episode-boundary-field") + 1] == "session_id"
    assert command[command.index("--input") + 1] == str(raw_prepared)
    assert command[command.index("--output") + 1] == str(indexed_prepared)
    assert command[command.index("--artifacts-dir") + 1] == str(artifacts)


def test_extraction_command_delegates_to_shared_rust_builder(monkeypatch, tmp_path):
    config = RunConfig(
        memory_mode="extracted",
        phase="full",
        dataset=tmp_path / "locomo.json",
        run_dir=tmp_path / "run",
    )
    raw_prepared = config.run_dir / "raw_prepared.json"
    indexed_prepared = config.run_dir / "extracted_prepared.json"
    artifacts = config.run_dir / "artifacts"
    calls = []

    def fake_builder(shared_config, raw, extracted, artifacts_dir):
        calls.append((shared_config, raw, extracted, artifacts_dir))
        return ["shared-command"]

    monkeypatch.setattr(locomo_run, "build_memory_pipeline_command", fake_builder)

    command = build_extraction_command(
        config, raw_prepared, indexed_prepared, artifacts, "config-digest-abc"
    )

    assert command == ["shared-command"]
    shared_config, raw, extracted, artifacts_dir = calls[0]
    assert shared_config.project_root == locomo_run.PROJECT_ROOT
    assert shared_config.cache_dir == config.run_dir / "cache" / "memory-pipeline"
    assert shared_config.cache_version == "config-digest-abc"
    assert shared_config.model == config.chat_model
    assert shared_config.verifier_model == config.chat_model
    assert shared_config.api_key_env == config.credential_env
    assert shared_config.base_url == config.base_url
    assert shared_config.episode_boundary_fields == ("session_id",)
    assert shared_config.fail_fast is False
    assert (raw, extracted, artifacts_dir) == (
        raw_prepared,
        indexed_prepared,
        artifacts,
    )


def test_search_command_enables_resume_and_rerank(tmp_path) -> None:
    config = RunConfig(
        memory_mode="raw",
        phase="full",
        dataset=tmp_path / "locomo.json",
        run_dir=tmp_path / "run",
    )
    store = tmp_path / "run" / "store.sqlite"
    indexed_prepared = tmp_path / "run" / "raw_prepared.json"
    search_results = tmp_path / "run" / "search_results.json"

    command = build_search_command(config, store, indexed_prepared, search_results)

    assert "--resume" in command
    assert "--rerank" in command
    assert command[command.index("--rerank-model") + 1] == "cohere/rerank-v3.5"
    assert command[command.index("--rerank-input-k") + 1] == "40"
    assert command[command.index("--top-k") + 1] == "30"
    assert command[command.index("--output") + 1] == str(search_results)
    assert command[command.index("--dataset") + 1] == str(indexed_prepared)


def test_graph_commands_use_one_explicit_configuration(tmp_path) -> None:
    config = RunConfig.from_env(
        {
            "MEMORY_MODE": "raw",
            "PHASE": "full",
            "DATASET": str(tmp_path / "locomo.json"),
            "RUN_DIR": str(tmp_path / "run"),
            "MEMORY_BENCH_GRAPH": "1",
            "GRAPH_RERANK": "1",
            "GRAPH_ALLOW_GRAPH_ONLY": "1",
            "GRAPH_MAX_GRAPH_ONLY_RESULTS": "4",
            "GRAPH_BUILD_CONCURRENCY": "3",
        }
    )
    store = tmp_path / "run" / "store.sqlite"
    prepared = tmp_path / "run" / "raw_prepared.json"
    results = tmp_path / "run" / "search_results.json"

    add = build_add_command(config, store, prepared)
    search = build_search_command(config, store, prepared, results)

    assert "--graph-build" in add
    assert add[add.index("--graph-build-concurrency") + 1] == "3"
    assert "--graph" in search
    assert "--graph-rerank" in search
    assert "--graph-allow-graph-only" in search
    assert search[search.index("--graph-max-graph-only-results") + 1] == "4"
    assert search[search.index("--graph-weight") + 1] == "0.2"
    assert search[search.index("--graph-memory-space-field") + 1] == "scope_id"


def test_graph_is_disabled_by_default(tmp_path, monkeypatch) -> None:
    for name in (
        "MEMORY_BENCH_GRAPH",
        "GRAPH_WEIGHT",
        "GRAPH_RERANK",
        "GRAPH_ALLOW_GRAPH_ONLY",
        "GRAPH_MAX_GRAPH_ONLY_RESULTS",
        "GRAPH_FAIL_OPEN",
    ):
        monkeypatch.delenv(name, raising=False)
    config = RunConfig.from_env(
        {
            "MEMORY_MODE": "raw",
            "PHASE": "full",
            "DATASET": str(tmp_path / "locomo.json"),
            "RUN_DIR": str(tmp_path / "run"),
        }
    )
    store = tmp_path / "run" / "store.sqlite"
    prepared = tmp_path / "run" / "raw_prepared.json"
    results = tmp_path / "run" / "search_results.json"

    assert "--graph-build" not in build_add_command(config, store, prepared)
    assert "--graph" not in build_search_command(config, store, prepared, results)


def test_stage_resumes_only_when_hashes_and_outputs_match(tmp_path) -> None:
    output = tmp_path / "prepared.json"
    upstream = tmp_path / "source.json"
    upstream.write_text('{"source": 1}\n', encoding="utf-8")
    calls = []

    def fake_runner(command, **kwargs):
        calls.append((command, kwargs))
        output.write_text('{"ok": true}\n', encoding="utf-8")
        return subprocess.CompletedProcess(command, 0)

    manifest = stage_manifest("adapter", source_hash="a", config_hash="b")
    run_stage(
        "adapter",
        ["fake", "--output", str(output)],
        (output,),
        manifest,
        inputs=(upstream,),
        runner=fake_runner,
    )
    run_stage(
        "adapter",
        ["fake", "--output", str(output)],
        (output,),
        manifest,
        inputs=(upstream,),
        runner=fake_runner,
    )
    assert len(calls) == 1

    output.write_text('{"changed": true}\n', encoding="utf-8")
    run_stage(
        "adapter",
        ["fake", "--output", str(output)],
        (output,),
        manifest,
        inputs=(upstream,),
        runner=fake_runner,
    )
    assert len(calls) == 2

    upstream.write_text('{"source": 2}\n', encoding="utf-8")
    run_stage(
        "adapter",
        ["fake", "--output", str(output)],
        (output,),
        manifest,
        inputs=(upstream,),
        runner=fake_runner,
    )
    assert len(calls) == 3
    completed = json.loads(
        (tmp_path / "stages" / "adapter.complete.json").read_text(encoding="utf-8")
    )
    assert completed["stage"] == "adapter"
    assert completed["inputs"][str(upstream)]


def test_failed_stage_does_not_publish_completion_manifest(tmp_path) -> None:
    output = tmp_path / "extracted.json"

    def failing_runner(command, **kwargs):
        output.write_text("partial", encoding="utf-8")
        raise subprocess.CalledProcessError(1, command)

    manifest = stage_manifest("extract", source_hash="a", config_hash="b")
    with pytest.raises(subprocess.CalledProcessError):
        run_stage(
            "extract",
            ["fake"],
            (output,),
            manifest,
            runner=failing_runner,
        )

    assert not (tmp_path / "stages" / "extract.complete.json").exists()


def test_invalidated_store_stage_rebuilds_instead_of_resuming_stale_ids(tmp_path) -> None:
    store = tmp_path / "store.sqlite"
    store.write_text("stale", encoding="utf-8")

    def rebuilding_runner(command, **kwargs):
        assert not store.exists()
        store.write_text("rebuilt", encoding="utf-8")
        return subprocess.CompletedProcess(command, 0)

    run_stage(
        "add",
        ["fake-add"],
        (store,),
        stage_manifest("add", "source", "config"),
        clean_outputs_on_rerun=True,
        runner=rebuilding_runner,
    )

    assert store.read_text(encoding="utf-8") == "rebuilt"


def test_run_directory_rejects_switching_between_raw_and_extracted(tmp_path) -> None:
    ensure_run_mode(tmp_path, "raw")
    ensure_run_mode(tmp_path, "raw")

    with pytest.raises(ValueError, match="already belongs to memory mode raw"):
        ensure_run_mode(tmp_path, "extracted")


def test_implementation_hash_tracks_memory_pipeline_binary(tmp_path, monkeypatch) -> None:
    binary = tmp_path / "memory-pipeline"
    binary.write_bytes(b"first")
    monkeypatch.setenv("MEMORY_PIPELINE_BIN", str(binary))
    first = locomo_run.implementation_hash()

    binary.write_bytes(b"second")

    assert locomo_run.implementation_hash() != first
