"""Command construction for the Rust memory extraction pipeline."""

from __future__ import annotations

from dataclasses import dataclass
import os
from pathlib import Path
import shlex


@dataclass(frozen=True)
class MemoryPipelineCommandConfig:
    project_root: Path
    cache_dir: Path
    cache_version: str
    model: str | None = None
    verifier_model: str | None = None
    api_key_env: str | None = None
    base_url: str | None = None
    extractor_responses: Path | None = None
    grounding_responses: Path | None = None
    max_candidate_tokens: int = 320
    max_window_tokens: int = 640
    context_before_messages: int = 2
    context_after_messages: int = 0
    episode_boundary_fields: tuple[str, ...] = ("session_id",)
    fail_fast: bool = False


def build_memory_pipeline_command(
    config: MemoryPipelineCommandConfig,
    raw_prepared: Path,
    extracted_prepared: Path,
    artifacts_dir: Path,
) -> list[str]:
    """Build one live-provider or paired-fixture pipeline command."""
    binary_override = os.getenv("MEMORY_PIPELINE_BIN")
    prefix = (
        shlex.split(binary_override)
        if binary_override
        else [
            "cargo",
            "run",
            "--quiet",
            "--manifest-path",
            str(config.project_root / "Cargo.toml"),
            "-p",
            "memory-pipeline",
            "--",
        ]
    )
    command = [
        *prefix,
        "--input",
        str(raw_prepared),
        "--output",
        str(extracted_prepared),
        "--artifacts-dir",
        str(artifacts_dir),
    ]

    has_extractor_fixture = config.extractor_responses is not None
    has_grounding_fixture = config.grounding_responses is not None
    if has_extractor_fixture != has_grounding_fixture:
        raise ValueError(
            "extractor and grounding response fixtures must both be provided"
        )
    if has_extractor_fixture:
        command.extend(
            [
                "--extractor-responses",
                str(config.extractor_responses),
                "--grounding-responses",
                str(config.grounding_responses),
            ]
        )
    else:
        provider_settings = {
            "model": config.model,
            "verifier_model": config.verifier_model,
            "api_key_env": config.api_key_env,
            "base_url": config.base_url,
        }
        missing = [name for name, value in provider_settings.items() if not value]
        if missing:
            raise ValueError(
                "live provider settings are required: " + ", ".join(missing)
            )
        command.extend(
            [
                "--model",
                config.model,
                "--verifier-model",
                config.verifier_model,
                "--api-key-env",
                config.api_key_env,
                "--base-url",
                config.base_url,
            ]
        )

    command.extend(
        [
            "--cache-dir",
            str(config.cache_dir),
            "--cache-version",
            config.cache_version,
        ]
    )
    for field in config.episode_boundary_fields:
        command.extend(["--episode-boundary-field", field])
    command.extend(
        [
            "--max-candidate-tokens",
            str(config.max_candidate_tokens),
            "--max-window-tokens",
            str(config.max_window_tokens),
            "--context-before-messages",
            str(config.context_before_messages),
            "--context-after-messages",
            str(config.context_after_messages),
            "--fail-fast" if config.fail_fast else "--no-fail-fast",
        ]
    )
    return command
