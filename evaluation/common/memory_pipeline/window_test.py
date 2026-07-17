from __future__ import annotations

import pytest

from common.memory_pipeline.models import ConversationEpisode, NormalizedMessage
from common.memory_pipeline.window import WindowConfig, build_windows, render_window


def _message(message_id: str, text: str) -> NormalizedMessage:
    return NormalizedMessage(
        id=message_id,
        scope_id="u1",
        session_id="s1",
        text=text,
        role="user",
        speaker="Alice",
    )


def _episode(*message_ids: str, episode_id: str = "episode-a") -> ConversationEpisode:
    return ConversationEpisode(
        id=episode_id,
        scope_id="u1",
        session_id="s1",
        message_ids=tuple(message_ids),
    )


def test_context_overlap_never_duplicates_candidate_ownership() -> None:
    lookup = {
        "m1": _message("m1", "one two"),
        "m2": _message("m2", "three four"),
        "m3": _message("m3", "five six"),
        "m4": _message("m4", "seven eight"),
    }

    windows = build_windows(
        [_episode("m1", "m2", "m3", "m4")],
        lookup,
        WindowConfig(
            max_candidate_tokens=4,
            max_window_tokens=8,
            context_before_messages=1,
        ),
    )

    candidate_ids = [
        ref.message_id
        for window in windows
        for ref in window.candidate_refs
    ]
    assert candidate_ids == ["m1", "m2", "m3", "m4"]
    assert len(candidate_ids) == len(set(candidate_ids))
    assert [ref.message_id for ref in windows[1].context_before_refs] == ["m2"]
    assert windows[0].context_before_refs == ()


def test_context_is_trimmed_before_candidate_when_total_budget_is_tight() -> None:
    lookup = {
        "m1": _message("m1", "one two three"),
        "m2": _message("m2", "four five six"),
    }

    windows = build_windows(
        [_episode("m1", "m2")],
        lookup,
        WindowConfig(
            max_candidate_tokens=3,
            max_window_tokens=4,
            context_before_messages=1,
        ),
    )

    assert windows[1].context_before_refs == ()
    assert [ref.message_id for ref in windows[1].candidate_refs] == ["m2"]


def test_oversized_message_is_sliced_without_changing_source_text() -> None:
    source = _message("long", "第一句。第二句。第三句。")

    windows = build_windows(
        [_episode("long")],
        {"long": source},
        WindowConfig(max_candidate_tokens=4, max_window_tokens=6),
    )

    refs = [ref for window in windows for ref in window.candidate_refs]
    assert len(refs) == 3
    assert "".join(source.text[ref.start_char:ref.end_char] for ref in refs) == source.text
    assert all(ref.text == source.text[ref.start_char:ref.end_char] for ref in refs)


def test_window_id_does_not_depend_on_episode_id() -> None:
    lookup = {"m1": _message("m1", "one two")}
    config = WindowConfig(max_candidate_tokens=4, max_window_tokens=4)

    first = build_windows([_episode("m1", episode_id="episode-a")], lookup, config)
    second = build_windows([_episode("m1", episode_id="episode-b")], lookup, config)

    assert first[0].id == second[0].id
    assert first[0].episode_id != second[0].episode_id


def test_render_window_marks_context_and_candidate_with_source_ids() -> None:
    lookup = {
        "m1": _message("m1", "Alice moved."),
        "m2": _message("m2", "She likes Hangzhou."),
    }
    windows = build_windows(
        [_episode("m1", "m2")],
        lookup,
        WindowConfig(
            max_candidate_tokens=3,
            max_window_tokens=10,
            context_before_messages=1,
        ),
    )

    rendered = render_window(windows[1], lookup)

    assert "<context>" in rendered
    assert "<candidate>" in rendered
    assert "message_id=m1" in rendered
    assert "message_id=m2" in rendered


def test_window_config_rejects_invalid_budgets() -> None:
    with pytest.raises(ValueError, match="max_window_tokens"):
        WindowConfig(max_candidate_tokens=10, max_window_tokens=9)
