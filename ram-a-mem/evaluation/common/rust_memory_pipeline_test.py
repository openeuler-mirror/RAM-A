from __future__ import annotations

from dataclasses import replace

import pytest

from common.rust_memory_pipeline import (
    MemoryPipelineCommandConfig,
    build_memory_pipeline_command,
)


def _config(tmp_path):
    return MemoryPipelineCommandConfig(
        project_root=tmp_path / "project",
        cache_dir=tmp_path / "cache",
        cache_version="cache-v1",
        model="extract-model",
        verifier_model="verify-model",
        api_key_env="PROVIDER_API_KEY",
        base_url="https://provider.example/v1",
    )


def _command(config, tmp_path):
    return build_memory_pipeline_command(
        config,
        tmp_path / "raw.json",
        tmp_path / "out.json",
        tmp_path / "artifacts",
    )


def test_rust_command_uses_binary_override(monkeypatch, tmp_path):
    monkeypatch.setenv(
        "MEMORY_PIPELINE_BIN", "/opt/ram-a/memory-pipeline --quiet"
    )

    command = _command(_config(tmp_path), tmp_path)

    assert command[:2] == ["/opt/ram-a/memory-pipeline", "--quiet"]
    assert "--no-fail-fast" in command


def test_rust_command_defaults_to_cargo_and_emits_live_provider(tmp_path):
    config = _config(tmp_path)

    command = _command(config, tmp_path)

    assert command[:8] == [
        "cargo",
        "run",
        "--quiet",
        "--manifest-path",
        str(config.project_root / "Cargo.toml"),
        "-p",
        "memory-pipeline",
        "--",
    ]
    assert command[command.index("--model") + 1] == "extract-model"
    assert command[command.index("--verifier-model") + 1] == "verify-model"
    assert command[command.index("--api-key-env") + 1] == "PROVIDER_API_KEY"
    assert command[command.index("--base-url") + 1] == "https://provider.example/v1"
    assert "--extractor-responses" not in command
    assert "--grounding-responses" not in command


@pytest.mark.parametrize(
    "missing_field",
    ["model", "verifier_model", "api_key_env", "base_url"],
)
def test_live_mode_requires_all_provider_settings(tmp_path, missing_field):
    config = replace(_config(tmp_path), **{missing_field: None})

    with pytest.raises(ValueError, match="live provider settings"):
        _command(config, tmp_path)


def test_fixture_mode_requires_both_response_maps(tmp_path):
    config = replace(
        _config(tmp_path), extractor_responses=tmp_path / "extract.json"
    )

    with pytest.raises(
        ValueError, match="extractor and grounding response fixtures"
    ):
        _command(config, tmp_path)


def test_fixture_mode_emits_only_paired_fixture_provider_arguments(tmp_path):
    extractor = tmp_path / "extract.json"
    grounding = tmp_path / "ground.json"
    config = replace(
        _config(tmp_path),
        extractor_responses=extractor,
        grounding_responses=grounding,
    )

    command = _command(config, tmp_path)

    assert command[command.index("--extractor-responses") + 1] == str(extractor)
    assert command[command.index("--grounding-responses") + 1] == str(grounding)
    for live_flag in ("--model", "--verifier-model", "--api-key-env", "--base-url"):
        assert live_flag not in command


def test_rust_command_repeats_episode_boundaries(tmp_path):
    config = replace(
        _config(tmp_path),
        episode_boundary_fields=("session_id", "conversation_id"),
    )

    command = _command(config, tmp_path)

    assert [
        command[index + 1]
        for index, value in enumerate(command)
        if value == "--episode-boundary-field"
    ] == ["session_id", "conversation_id"]


@pytest.mark.parametrize(
    ("fail_fast", "expected", "unexpected"),
    [(True, "--fail-fast", "--no-fail-fast"), (False, "--no-fail-fast", "--fail-fast")],
)
def test_rust_command_emits_exactly_one_fail_fast_choice(
    tmp_path, fail_fast, expected, unexpected
):
    command = _command(replace(_config(tmp_path), fail_fast=fail_fast), tmp_path)

    assert command.count(expected) == 1
    assert unexpected not in command
