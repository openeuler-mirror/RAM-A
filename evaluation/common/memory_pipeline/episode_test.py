from __future__ import annotations

from common.memory_pipeline.episode import EpisodeConfig, build_episodes
from common.memory_pipeline.models import NormalizedMessage


def _message(
    message_id: str,
    scope_id: str = "u1",
    session_id: str = "s1",
    timestamp: str = "",
    *,
    topic: str = "",
) -> NormalizedMessage:
    metadata = {"topic": topic} if topic else {}
    return NormalizedMessage(
        id=message_id,
        scope_id=scope_id,
        session_id=session_id,
        timestamp=timestamp,
        text=f"text for {message_id}",
        role="user",
        metadata=metadata,
    )


def test_episode_builder_splits_scope_session_and_time_gap() -> None:
    messages = [
        _message("m1", "u1", "s1", "2026-07-01T10:00:00Z"),
        _message("m2", "u1", "s1", "2026-07-01T10:05:00Z"),
        _message("m3", "u1", "s1", "2026-07-01T12:00:00Z"),
        _message("m4", "u1", "s2", "2026-07-01T12:01:00Z"),
        _message("m5", "u2", "s2", "2026-07-01T12:02:00Z"),
    ]

    episodes = build_episodes(messages, EpisodeConfig(max_time_gap_minutes=30))

    assert [episode.message_ids for episode in episodes] == [
        ("m1", "m2"),
        ("m3",),
        ("m4",),
        ("m5",),
    ]
    assert [episode.boundary_reason for episode in episodes] == [
        "start",
        "time_gap",
        "session_change",
        "scope_change",
    ]


def test_episode_builder_splits_configured_metadata_boundary() -> None:
    messages = [
        _message("m1", topic="travel"),
        _message("m2", topic="travel"),
        _message("m3", topic="work"),
    ]

    episodes = build_episodes(
        messages,
        EpisodeConfig(metadata_boundary_fields=("topic",)),
    )

    assert [episode.message_ids for episode in episodes] == [("m1", "m2"), ("m3",)]
    assert episodes[1].boundary_reason == "metadata_change:topic"


def test_invalid_timestamp_does_not_reorder_or_create_gap() -> None:
    messages = [
        _message("m1", timestamp="not-a-time"),
        _message("m2", timestamp="2026-07-01T12:00:00Z"),
    ]

    episodes = build_episodes(messages, EpisodeConfig(max_time_gap_minutes=1))

    assert len(episodes) == 1
    assert episodes[0].message_ids == ("m1", "m2")


def test_episode_output_is_byte_stable() -> None:
    messages = [_message("m1"), _message("m2")]
    config = EpisodeConfig()

    first = [episode.to_dict() for episode in build_episodes(messages, config)]
    second = [episode.to_dict() for episode in build_episodes(messages, config)]

    assert first == second
    assert first[0]["id"].startswith("episode-")


def test_source_text_change_changes_episode_id() -> None:
    original = [_message("m1")]
    changed = [
        NormalizedMessage(
            id="m1",
            scope_id="u1",
            session_id="s1",
            text="changed text",
            role="user",
        )
    ]

    assert build_episodes(original, EpisodeConfig())[0].id != build_episodes(
        changed,
        EpisodeConfig(),
    )[0].id
