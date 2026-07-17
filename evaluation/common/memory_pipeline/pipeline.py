"""End-to-end orchestration for evidence-grounded memory preparation."""

from __future__ import annotations

from dataclasses import asdict, dataclass, field
import json
from pathlib import Path
from typing import Any, Mapping, Sequence

from .cache import JsonCache
from .canonical import stable_hash
from .episode import EpisodeConfig, build_episodes
from .extraction import SCHEMA_VERSION, ExtractionBatch, MemoryExtractor, ModelUsage
from .grounding import (
    GroundingBatch,
    GroundingResult,
    GroundingVerifier,
)
from .models import (
    AtomicMemory,
    ConversationEpisode,
    ExtractionWindow,
    NormalizedMessage,
    PipelineIssue,
)
from .normalize import NORMALIZER_VERSION, normalize_prepared_memories
from .validation import ValidationConfig, validate_extraction
from .window import WindowConfig, build_windows
from .writer import aggregate_exact_memories, make_prepared_output


@dataclass(frozen=True)
class PipelineConfig:
    episode: EpisodeConfig = field(default_factory=EpisodeConfig)
    window: WindowConfig = field(default_factory=WindowConfig)
    validation: ValidationConfig = field(default_factory=ValidationConfig)
    pipeline_version: str = "memory_pipeline_v1"
    fail_fast: bool = True


@dataclass(frozen=True)
class PipelineRun:
    prepared: dict[str, Any]
    normalized_messages: list[NormalizedMessage]
    episodes: list[ConversationEpisode]
    windows: list[ExtractionWindow]
    extracted_candidates: list[ExtractionBatch]
    accepted_memories: list[AtomicMemory]
    rejected: list[PipelineIssue]
    quarantined: list[PipelineIssue]
    stats: dict[str, Any]
    run_metadata: dict[str, Any]


def run_memory_pipeline(
    prepared: dict[str, Any],
    config: PipelineConfig,
    extractor: MemoryExtractor,
    verifier: GroundingVerifier,
    cache: JsonCache | None = None,
) -> PipelineRun:
    """Prepare grounded atomic memories while preserving benchmark queries."""

    messages, normalization_issues = normalize_prepared_memories(prepared)
    messages_by_id = {message.id: message for message in messages}
    episodes = build_episodes(messages, config.episode)
    windows = build_windows(episodes, messages_by_id, config.window)

    extracted_candidates: list[ExtractionBatch] = []
    supported: list[AtomicMemory] = []
    rejected = list(normalization_issues)
    quarantined: list[PipelineIssue] = []
    extraction_tokens = 0
    verification_tokens = 0
    empty_windows = 0
    extraction_cache_hits = 0
    verification_cache_hits = 0
    extraction_call_count = 0
    verification_call_count = 0
    extraction_latency_ms = 0.0
    verification_latency_ms = 0.0
    grounding_status_counts: dict[str, int] = {}

    for window in windows:
        try:
            extraction, was_cached = _extract(
                window, messages_by_id, extractor, cache
            )
        except Exception as error:
            if config.fail_fast:
                raise
            rejected.append(_runtime_issue("extract", window, error))
            continue

        extracted_candidates.append(extraction)
        if not was_cached:
            extraction_call_count += 1
            extraction_latency_ms += extraction.usage.latency_ms
            extraction_tokens += extraction.usage.total_tokens
        extraction_cache_hits += int(was_cached)
        if not extraction.raw_memories:
            empty_windows += 1
            continue

        validation = validate_extraction(
            extraction.raw_memories,
            window,
            messages_by_id,
            config.validation,
        )
        rejected.extend(validation.rejected)
        quarantined.extend(validation.quarantined)
        if not validation.valid:
            continue

        try:
            grounding, was_cached = _verify(
                window, validation.valid, messages_by_id, verifier, cache
            )
        except Exception as error:
            if config.fail_fast:
                raise
            quarantined.extend(
                _runtime_issue("grounding", window, error, candidate.id)
                for candidate in validation.valid
            )
            continue

        if not was_cached:
            verification_call_count += 1
            verification_latency_ms += grounding.usage.latency_ms
            verification_tokens += grounding.usage.total_tokens
        verification_cache_hits += int(was_cached)
        results_by_id = {result.memory_id: result for result in grounding.results}
        for memory in validation.valid:
            result = results_by_id[memory.id]
            grounding_status_counts[result.status] = (
                grounding_status_counts.get(result.status, 0) + 1
            )
            if result.status == "SUPPORTED":
                supported.append(memory)
            else:
                quarantined.append(
                    PipelineIssue(
                        stage="grounding",
                        code=f"grounding_{result.status.lower()}",
                        message=result.reason or f"grounding status is {result.status}",
                        source_id=memory.id,
                        scope_id=memory.scope_id,
                        episode_id=memory.source_episode_id,
                        window_id=memory.source_window_id,
                        details={"status": result.status},
                    )
                )

    accepted = aggregate_exact_memories(supported)
    source_turn_memory_counts: dict[str, int] = {
        message_id: 0 for message_id in messages_by_id
    }
    source_turn_evidence_ref_counts: dict[str, int] = {
        message_id: 0 for message_id in messages_by_id
    }
    for memory in accepted:
        for message_id in {ref.message_id for ref in memory.evidence}:
            source_turn_memory_counts[message_id] = (
                source_turn_memory_counts.get(message_id, 0) + 1
            )
        for ref in memory.evidence:
            source_turn_evidence_ref_counts[ref.message_id] = (
                source_turn_evidence_ref_counts.get(ref.message_id, 0) + 1
            )
    run_metadata = {
        "pipeline_version": config.pipeline_version,
        "dataset": _json_value(prepared.get("dataset", {})),
        "source_hash": stable_hash(prepared),
        "normalizer_version": NORMALIZER_VERSION,
        "config": _json_value(asdict(config)),
        "extractor": {
            "model": extractor.model,
            "prompt_version": extractor.prompt_version,
            "schema_version": SCHEMA_VERSION,
            "implementation": type(extractor).__qualname__,
        },
        "verifier": {
            "model": verifier.model,
            "prompt_version": verifier.prompt_version,
            "implementation": type(verifier).__qualname__,
        },
        "cache_version": cache.version if cache is not None else None,
    }
    coverage, duplication = _candidate_span_metrics(messages, windows)
    stats = {
        "normalized_message_count": len(messages),
        "episode_count": len(episodes),
        "window_count": len(windows),
        "empty_extraction_windows": empty_windows,
        "raw_candidate_count": sum(
            len(batch.raw_memories) for batch in extracted_candidates
        ),
        "accepted_memory_count": len(accepted),
        "rejected_count": len(rejected),
        "quarantined_count": len(quarantined),
        "candidate_source_coverage": coverage,
        "candidate_source_duplication": duplication,
        "extraction_total_tokens": extraction_tokens,
        "verification_total_tokens": verification_tokens,
        "extraction_cache_hits": extraction_cache_hits,
        "verification_cache_hits": verification_cache_hits,
        "extraction_call_count": extraction_call_count,
        "verification_call_count": verification_call_count,
        "extraction_latency_ms": extraction_latency_ms,
        "verification_latency_ms": verification_latency_ms,
        "grounding_status_counts": grounding_status_counts,
        "source_turn_memory_counts": source_turn_memory_counts,
        "source_turn_evidence_ref_counts": source_turn_evidence_ref_counts,
    }
    output = make_prepared_output(prepared, accepted, run_metadata)
    return PipelineRun(
        prepared=output,
        normalized_messages=messages,
        episodes=episodes,
        windows=windows,
        extracted_candidates=extracted_candidates,
        accepted_memories=accepted,
        rejected=rejected,
        quarantined=quarantined,
        stats=stats,
        run_metadata=run_metadata,
    )


def write_pipeline_artifacts(run: PipelineRun, artifact_dir: Path) -> None:
    """Write a deterministic audit bundle for one pipeline run."""

    root = Path(artifact_dir)
    root.mkdir(parents=True, exist_ok=True)
    _write_jsonl(
        root / "normalized_messages.jsonl",
        [item.to_dict() for item in run.normalized_messages],
    )
    _write_jsonl(root / "episodes.jsonl", [item.to_dict() for item in run.episodes])
    _write_jsonl(
        root / "extraction_windows.jsonl",
        [item.to_dict() for item in run.windows],
    )
    _write_jsonl(
        root / "extracted_candidates.jsonl",
        [item.to_dict() for item in run.extracted_candidates],
    )
    _write_jsonl(
        root / "accepted_memories.jsonl",
        [item.to_dict() for item in run.accepted_memories],
    )
    _write_jsonl(
        root / "rejected_extractions.jsonl",
        [item.to_dict() for item in run.rejected],
    )
    _write_jsonl(
        root / "quarantined_memories.jsonl",
        [item.to_dict() for item in run.quarantined],
    )
    _write_json(root / "extraction_stats.json", run.stats)
    _write_json(root / "run_metadata.json", run.run_metadata)
    _write_json(root / "prepared.json", run.prepared)


def _extract(
    window: ExtractionWindow,
    messages_by_id: Mapping[str, NormalizedMessage],
    extractor: MemoryExtractor,
    cache: JsonCache | None,
) -> tuple[ExtractionBatch, bool]:
    key = [
        window.to_dict(),
        _window_messages(window, messages_by_id),
        _component_identity(extractor),
        SCHEMA_VERSION,
    ]
    if cache is not None:
        cached = cache.get("extraction", key)
        if cached is not None:
            return _extraction_from_dict(cached), True
    batch = extractor.extract(window, messages_by_id)
    if cache is not None:
        cache.put("extraction", key, batch.to_dict())
    return batch, False


def _verify(
    window: ExtractionWindow,
    memories: Sequence[AtomicMemory],
    messages_by_id: Mapping[str, NormalizedMessage],
    verifier: GroundingVerifier,
    cache: JsonCache | None,
) -> tuple[GroundingBatch, bool]:
    key = [
        window.id,
        [memory.to_dict() for memory in memories],
        _evidence_messages(memories, messages_by_id),
        _component_identity(verifier),
    ]
    if cache is not None:
        cached = cache.get("grounding", key)
        if cached is not None:
            return _grounding_from_dict(cached), True
    batch = verifier.verify(window, memories, messages_by_id)
    if cache is not None:
        cache.put("grounding", key, batch.to_dict())
    return batch, False


def _extraction_from_dict(value: Mapping[str, Any]) -> ExtractionBatch:
    usage = value.get("usage") or {}
    return ExtractionBatch(
        window_id=str(value["window_id"]),
        schema_version=str(value["schema_version"]),
        raw_memories=list(value["raw_memories"]),
        usage=_usage_from_dict(usage),
        raw_response=str(value.get("raw_response") or ""),
    )


def _grounding_from_dict(value: Mapping[str, Any]) -> GroundingBatch:
    usage = value.get("usage") or {}
    return GroundingBatch(
        window_id=str(value["window_id"]),
        results=[
            GroundingResult(
                memory_id=str(item["memory_id"]),
                status=str(item["status"]),
                reason=str(item.get("reason") or ""),
            )
            for item in value["results"]
        ],
        usage=_usage_from_dict(usage),
        raw_response=str(value.get("raw_response") or ""),
    )


def _usage_from_dict(value: Mapping[str, Any]) -> ModelUsage:
    return ModelUsage(
        latency_ms=float(value.get("latency_ms") or 0),
        prompt_tokens=int(value.get("prompt_tokens") or 0),
        completion_tokens=int(value.get("completion_tokens") or 0),
        total_tokens=int(value.get("total_tokens") or 0),
    )


def _window_messages(
    window: ExtractionWindow,
    messages_by_id: Mapping[str, NormalizedMessage],
) -> list[dict[str, Any]]:
    refs = (
        *window.context_before_refs,
        *window.candidate_refs,
        *window.context_after_refs,
    )
    ordered_ids = dict.fromkeys(ref.message_id for ref in refs)
    return [messages_by_id[message_id].to_dict() for message_id in ordered_ids]


def _component_identity(component: Any) -> dict[str, Any]:
    identity = {
        "implementation": type(component).__qualname__,
        "model": component.model,
        "prompt_version": component.prompt_version,
    }
    max_output_tokens = getattr(component, "max_output_tokens", None)
    if max_output_tokens is not None:
        identity["max_output_tokens"] = max_output_tokens
    return identity


def _evidence_messages(
    memories: Sequence[AtomicMemory],
    messages_by_id: Mapping[str, NormalizedMessage],
) -> list[dict[str, Any]]:
    ordered_ids = dict.fromkeys(
        evidence.message_id
        for memory in memories
        for evidence in memory.evidence
    )
    return [messages_by_id[message_id].to_dict() for message_id in ordered_ids]


def _runtime_issue(
    stage: str,
    window: ExtractionWindow,
    error: Exception,
    source_id: str = "",
) -> PipelineIssue:
    return PipelineIssue(
        stage=stage,
        code=f"{stage}_runtime_error",
        message=str(error),
        source_id=source_id,
        scope_id=window.scope_id,
        episode_id=window.episode_id,
        window_id=window.id,
        details={"error_type": type(error).__name__},
    )


def _candidate_span_metrics(
    messages: Sequence[NormalizedMessage],
    windows: Sequence[ExtractionWindow],
) -> tuple[float, int]:
    spans_by_message: dict[str, list[tuple[int, int]]] = {
        message.id: [] for message in messages
    }
    total_chars = sum(len(message.text) for message in messages)
    raw_span_chars = 0
    for window in windows:
        for ref in window.candidate_refs:
            spans_by_message.setdefault(ref.message_id, []).append(
                (ref.start_char, ref.end_char)
            )
            raw_span_chars += max(0, ref.end_char - ref.start_char)

    covered_chars = sum(
        _union_length(spans) for spans in spans_by_message.values()
    )
    coverage = covered_chars / total_chars if total_chars else 1.0
    return coverage, raw_span_chars - covered_chars


def _union_length(spans: Sequence[tuple[int, int]]) -> int:
    if not spans:
        return 0
    ordered = sorted(spans)
    total = 0
    start, end = ordered[0]
    for next_start, next_end in ordered[1:]:
        if next_start <= end:
            end = max(end, next_end)
        else:
            total += max(0, end - start)
            start, end = next_start, next_end
    return total + max(0, end - start)


def _write_jsonl(path: Path, values: Sequence[dict[str, Any]]) -> None:
    content = "".join(
        json.dumps(value, ensure_ascii=False, sort_keys=True) + "\n"
        for value in values
    )
    path.write_text(content, encoding="utf-8")


def _write_json(path: Path, value: Any) -> None:
    path.write_text(
        json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n",
        encoding="utf-8",
    )


def _json_value(value: Any) -> Any:
    """Normalize dataclass output to the same types produced by JSON parsing."""

    return json.loads(json.dumps(value, ensure_ascii=False, sort_keys=True))
