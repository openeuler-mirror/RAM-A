"""Command-line entry point for grounded atomic-memory preparation."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any, Sequence

from common.llm_client import OpenAICompatibleClient

from .cache import JsonCache
from .episode import EpisodeConfig
from .extraction import LLMMemoryExtractor, StaticMemoryExtractor
from .grounding import LLMGroundingVerifier, StaticGroundingVerifier
from .pipeline import PipelineConfig, run_memory_pipeline, write_pipeline_artifacts
from .validation import ValidationConfig
from .window import WindowConfig


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Convert raw benchmark-prepared-v1 messages into evidence-grounded "
            "atomic memories."
        )
    )
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--artifacts-dir", type=Path, required=True)

    fixture = parser.add_argument_group("offline fixture mode")
    fixture.add_argument("--extractor-responses", type=Path)
    fixture.add_argument("--grounding-responses", type=Path)

    live = parser.add_argument_group("live OpenAI-compatible mode")
    live.add_argument("--model")
    live.add_argument("--verifier-model")
    live.add_argument("--api-key-env")
    live.add_argument(
        "--base-url",
        default="https://openrouter.ai/api/v1",
    )
    live.add_argument("--timeout-seconds", type=int, default=120)
    live.add_argument("--max-retries", type=int, default=8)

    episode = parser.add_argument_group("episode configuration")
    episode.add_argument("--max-time-gap-minutes", type=int)
    episode.add_argument(
        "--episode-boundary-field",
        action="append",
        default=[],
        help="Metadata field that starts a new episode when its value changes.",
    )

    window = parser.add_argument_group("window configuration")
    window.add_argument("--max-candidate-tokens", type=int, default=320)
    window.add_argument("--max-window-tokens", type=int, default=640)
    window.add_argument("--context-before-messages", type=int, default=2)
    window.add_argument("--context-after-messages", type=int, default=0)

    runtime = parser.add_argument_group("runtime")
    runtime.add_argument("--cache-dir", type=Path)
    runtime.add_argument("--cache-version", default="cache_v1")
    runtime.add_argument(
        "--fail-fast",
        action=argparse.BooleanOptionalAction,
        default=True,
        help="Abort on a model-call error (default: enabled).",
    )
    runtime.add_argument("--max-memory-chars", type=int, default=500)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    _validate_mode(parser, args)

    prepared = _read_object(args.input, "input prepared file")
    if args.extractor_responses is not None:
        extractor = StaticMemoryExtractor(
            _read_object(args.extractor_responses, "extractor response file")
        )
        verifier = StaticGroundingVerifier(
            _read_object(args.grounding_responses, "grounding response file")
        )
    else:
        client = OpenAICompatibleClient(
            api_key_env=args.api_key_env,
            base_url=args.base_url,
            timeout_s=args.timeout_seconds,
            max_retries=args.max_retries,
        )
        extractor = LLMMemoryExtractor(client=client, model=args.model)
        verifier = LLMGroundingVerifier(client=client, model=args.verifier_model)

    config = PipelineConfig(
        episode=EpisodeConfig(
            max_time_gap_minutes=args.max_time_gap_minutes,
            metadata_boundary_fields=tuple(args.episode_boundary_field),
        ),
        window=WindowConfig(
            max_candidate_tokens=args.max_candidate_tokens,
            max_window_tokens=args.max_window_tokens,
            context_before_messages=args.context_before_messages,
            context_after_messages=args.context_after_messages,
        ),
        validation=ValidationConfig(max_memory_chars=args.max_memory_chars),
        fail_fast=args.fail_fast,
    )
    cache = (
        JsonCache(args.cache_dir, version=args.cache_version)
        if args.cache_dir is not None
        else None
    )
    run = run_memory_pipeline(prepared, config, extractor, verifier, cache=cache)
    write_pipeline_artifacts(run, args.artifacts_dir)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(run.prepared, ensure_ascii=False, sort_keys=True, indent=2) + "\n",
        encoding="utf-8",
    )
    return 0


def _validate_mode(parser: argparse.ArgumentParser, args: argparse.Namespace) -> None:
    fixture_values = (args.extractor_responses, args.grounding_responses)
    if any(value is not None for value in fixture_values):
        if not all(value is not None for value in fixture_values):
            parser.error(
                "fixture mode requires both --extractor-responses and "
                "--grounding-responses"
            )
        if any((args.model, args.verifier_model, args.api_key_env)):
            parser.error("fixture mode cannot be combined with live model arguments")
        return

    missing = [
        name
        for name, value in (
            ("--model", args.model),
            ("--verifier-model", args.verifier_model),
            ("--api-key-env", args.api_key_env),
        )
        if not value
    ]
    if missing:
        parser.error(
            "live mode requires " + ", ".join(missing) + "; alternatively use both fixture response files"
        )


def _read_object(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read {label} {path}: {error}") from error
    if not isinstance(value, dict):
        raise ValueError(f"{label} must contain one JSON object")
    return value


if __name__ == "__main__":
    raise SystemExit(main())
