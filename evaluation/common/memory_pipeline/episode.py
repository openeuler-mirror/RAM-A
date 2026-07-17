"""Deterministic partitioning of normalized messages into episodes."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Sequence

from .canonical import stable_hash
from .models import ConversationEpisode, NormalizedMessage


@dataclass(frozen=True)
class EpisodeConfig:
    max_time_gap_minutes: int | None = None
    metadata_boundary_fields: tuple[str, ...] = ()
    version: str = "episode_v1"

    def __post_init__(self) -> None:
        if self.max_time_gap_minutes is not None and self.max_time_gap_minutes < 0:
            raise ValueError("max_time_gap_minutes must be non-negative")


def build_episodes(
    messages: Sequence[NormalizedMessage],
    config: EpisodeConfig,
) -> list[ConversationEpisode]:
    if not messages:
        return []

    episodes: list[ConversationEpisode] = []
    current: list[NormalizedMessage] = []
    current_reason = "start"

    for message in messages:
        reason = _boundary_reason(current[-1], message, config) if current else None
        if reason:
            episodes.append(_make_episode(current, current_reason, config))
            current = []
            current_reason = reason
        current.append(message)

    episodes.append(_make_episode(current, current_reason, config))
    return episodes


def _boundary_reason(
    previous: NormalizedMessage,
    current: NormalizedMessage,
    config: EpisodeConfig,
) -> str | None:
    if previous.scope_id != current.scope_id:
        return "scope_change"
    if previous.session_id != current.session_id:
        return "session_change"

    for field_name in config.metadata_boundary_fields:
        if previous.metadata.get(field_name) != current.metadata.get(field_name):
            return f"metadata_change:{field_name}"

    if config.max_time_gap_minutes is not None:
        previous_time = _parse_time(previous.timestamp)
        current_time = _parse_time(current.timestamp)
        if previous_time is not None and current_time is not None:
            gap_minutes = (current_time - previous_time).total_seconds() / 60
            if gap_minutes > config.max_time_gap_minutes:
                return "time_gap"
    return None


def _make_episode(
    messages: Sequence[NormalizedMessage],
    boundary_reason: str,
    config: EpisodeConfig,
) -> ConversationEpisode:
    first = messages[0]
    refs = [
        {
            "message_id": message.id,
            "text_hash": stable_hash(message.text),
        }
        for message in messages
    ]
    episode_id = "episode-" + stable_hash(
        first.scope_id,
        first.session_id,
        refs,
        {
            "max_time_gap_minutes": config.max_time_gap_minutes,
            "metadata_boundary_fields": config.metadata_boundary_fields,
            "version": config.version,
        },
    )
    return ConversationEpisode(
        id=episode_id,
        scope_id=first.scope_id,
        session_id=first.session_id,
        message_ids=tuple(message.id for message in messages),
        start_time=first.timestamp,
        end_time=messages[-1].timestamp,
        boundary_reason=boundary_reason,
        episode_version=config.version,
    )


def _parse_time(value: str) -> datetime | None:
    if not value:
        return None
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)
