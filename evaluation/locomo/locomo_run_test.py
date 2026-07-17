from __future__ import annotations

import json
from pathlib import Path
import subprocess

import pytest

import locomo.locomo_run as locomo_run

from common.memory_pipeline.cache import JsonCache
from common.memory_pipeline.extraction import StaticMemoryExtractor
from common.memory_pipeline.grounding import StaticGroundingVerifier
from common.memory_pipeline.pipeline import PipelineConfig, run_memory_pipeline
from locomo.locomo_adapter import prepare_locomo
from locomo.locomo_provenance import render_contexts
from locomo.locomo_run import (
    RunConfig,
    build_extraction_command,
    build_search_command,
    config_hash,
    ensure_run_mode,
    memory_bench_base_command,
    run_stage,
    stage_manifest,
    validate_frozen_config,
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
            "PHASE": "pilot",
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


def test_memory_bench_command_contains_frozen_retrieval_configuration(tmp_path) -> None:
    config = RunConfig(
        memory_mode="raw",
        phase="pilot",
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
        phase="pilot",
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
    assert command[1:3] == ["-m", "common.memory_pipeline.cli"]
    assert "--model" in command
    assert command[command.index("--model") + 1] == "openai/gpt-4o-mini"
    assert command[command.index("--verifier-model") + 1] == "openai/gpt-4o-mini"
    assert command[command.index("--api-key-env") + 1] == "OPENROUTER_API_KEY"
    assert command[command.index("--cache-version") + 1] == "config-digest-abc"
    assert command[command.index("--episode-boundary-field") + 1] == "session_id"
    assert command[command.index("--input") + 1] == str(raw_prepared)
    assert command[command.index("--output") + 1] == str(indexed_prepared)
    assert command[command.index("--artifacts-dir") + 1] == str(artifacts)


def test_search_command_enables_resume_and_rerank(tmp_path) -> None:
    config = RunConfig(
        memory_mode="raw",
        phase="pilot",
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


def test_frozen_config_compares_only_immutable_experiment_fields(tmp_path) -> None:
    pilot = RunConfig(
        memory_mode="extracted",
        phase="pilot",
        dataset=tmp_path / "locomo.json",
        run_dir=tmp_path / "pilot",
    )
    full = RunConfig(
        memory_mode="extracted",
        phase="full",
        dataset=tmp_path / "locomo.json",
        run_dir=tmp_path / "full",
    )
    frozen = tmp_path / "frozen.json"
    frozen.write_text(
        json.dumps(pilot.public_manifest()),
        encoding="utf-8",
    )

    validate_frozen_config(full, frozen)

    changed = dict(pilot.public_manifest())
    changed["top_k"] = 29
    frozen.write_text(json.dumps(changed), encoding="utf-8")
    with pytest.raises(ValueError, match="frozen configuration mismatch"):
        validate_frozen_config(full, frozen)


def test_run_directory_rejects_switching_between_raw_and_extracted(tmp_path) -> None:
    ensure_run_mode(tmp_path, "raw")
    ensure_run_mode(tmp_path, "raw")

    with pytest.raises(ValueError, match="already belongs to memory mode raw"):
        ensure_run_mode(tmp_path, "extracted")


def test_offline_fixture_runs_pipeline_context_and_cache_resume(tmp_path) -> None:
    dataset = json.loads(FIXTURE.read_text(encoding="utf-8"))
    prepared = prepare_locomo(dataset)
    extractor = StaticMemoryExtractor(
        json.loads(EXTRACTOR_FIXTURE.read_text(encoding="utf-8"))
    )
    verifier = StaticGroundingVerifier(
        json.loads(GROUNDING_FIXTURE.read_text(encoding="utf-8"))
    )
    cache = JsonCache(tmp_path / "cache", version="offline-smoke-v1")

    first = run_memory_pipeline(
        prepared,
        PipelineConfig(),
        extractor,
        verifier,
        cache,
    )
    second = run_memory_pipeline(
        prepared,
        PipelineConfig(),
        extractor,
        verifier,
        cache,
    )

    assert first.stats["candidate_source_coverage"] == 1.0
    assert first.stats["accepted_memory_count"] >= 2
    assert second.stats["extraction_call_count"] == 0
    assert second.stats["verification_call_count"] == 0
    assert second.prepared == first.prepared
    query = first.prepared["queries"][0]
    item = {
        "query_path": "$.queries[0].text",
        "query": query["text"],
        "query_id": query["id"],
        "filter": query["filter"],
        "metadata": query["metadata"],
        "task": query["task"],
        "results": [{**first.prepared["memories"][0], "score": 1.0}],
    }
    contexts = render_contexts(dataset, prepared, item, "extracted")
    assert any(
        "[Atomic]" in row["memory"] and "[Evidence" in row["memory"]
        for rows in contexts.values()
        for row in rows
    )
