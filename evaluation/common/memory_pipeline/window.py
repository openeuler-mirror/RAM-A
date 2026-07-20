"""Candidate-owned extraction windows with optional overlapping context."""

from __future__ import annotations

from dataclasses import asdict, dataclass
import re
from typing import Mapping, Sequence

from .canonical import estimate_tokens, stable_hash
from .models import (
    ConversationEpisode,
    ExtractionWindow,
    MessageRef,
    NormalizedMessage,
)


_SENTENCE_END_RE = re.compile(r".*?(?:[。！？!?；;]|\.(?:\s|$)|\n+)", re.DOTALL)


@dataclass(frozen=True)
class WindowConfig:
    max_candidate_tokens: int = 320
    max_window_tokens: int = 640
    context_before_messages: int = 2
    context_after_messages: int = 0
    tokenizer_name: str = "heuristic"
    tokenizer_version: str = "heuristic_v1"
    version: str = "window_v1"

    def __post_init__(self) -> None:
        if self.max_candidate_tokens <= 0:
            raise ValueError("max_candidate_tokens must be positive")
        if self.max_window_tokens < self.max_candidate_tokens:
            raise ValueError("max_window_tokens must be >= max_candidate_tokens")
        if self.context_before_messages < 0 or self.context_after_messages < 0:
            raise ValueError("context message counts must be non-negative")


def build_windows(
    episodes: Sequence[ConversationEpisode],
    messages_by_id: Mapping[str, NormalizedMessage],
    config: WindowConfig,
) -> list[ExtractionWindow]:
    windows: list[ExtractionWindow] = []
    for episode in episodes:
        refs: list[MessageRef] = []
        for message_id in episode.message_ids:
            try:
                message = messages_by_id[message_id]
            except KeyError as error:
                raise ValueError(f"episode references unknown message: {message_id}") from error
            refs.extend(_slice_message(message, config.max_candidate_tokens))

        groups = _pack_candidate_refs(refs, config.max_candidate_tokens)
        ref_positions = {id(ref): index for index, ref in enumerate(refs)}
        for group in groups:
            start = ref_positions[id(group[0])]
            end = ref_positions[id(group[-1])] + 1
            before = _select_context_before(
                refs[:start],
                config.context_before_messages,
            )
            after = _select_context_after(
                refs[end:],
                config.context_after_messages,
            )
            before, after = _trim_context_to_budget(
                before,
                group,
                after,
                config.max_window_tokens,
            )
            windows.append(_make_window(episode, group, before, after, config))
    return windows


def render_window(
    window: ExtractionWindow,
    messages_by_id: Mapping[str, NormalizedMessage],
) -> str:
    return "\n".join(
        [
            "<context>",
            *(_render_ref(ref, messages_by_id) for ref in window.context_before_refs),
            *(_render_ref(ref, messages_by_id) for ref in window.context_after_refs),
            "</context>",
            "",
            "<candidate>",
            *(_render_ref(ref, messages_by_id) for ref in window.candidate_refs),
            "</candidate>",
        ]
    )


def _slice_message(message: NormalizedMessage, max_tokens: int) -> list[MessageRef]:
    if estimate_tokens(message.text) <= max_tokens:
        return [MessageRef(message.id, 0, len(message.text), message.text)]

    sentence_spans = _sentence_spans(message.text)
    refs: list[MessageRef] = []
    for start, end in sentence_spans:
        refs.extend(_split_span(message, start, end, max_tokens))
    return refs


def _sentence_spans(text: str) -> list[tuple[int, int]]:
    spans: list[tuple[int, int]] = []
    cursor = 0
    for match in _SENTENCE_END_RE.finditer(text):
        if match.start() > cursor:
            spans.append((cursor, match.start()))
        spans.append((match.start(), match.end()))
        cursor = match.end()
    if cursor < len(text):
        spans.append((cursor, len(text)))
    return [(start, end) for start, end in spans if end > start]


def _split_span(
    message: NormalizedMessage,
    start: int,
    end: int,
    max_tokens: int,
) -> list[MessageRef]:
    text = message.text
    if estimate_tokens(text[start:end]) <= max_tokens:
        return [MessageRef(message.id, start, end, text[start:end])]

    refs: list[MessageRef] = []
    piece_start = start
    cursor = start
    while cursor < end:
        next_cursor = cursor + 1
        if (
            cursor > piece_start
            and estimate_tokens(text[piece_start:next_cursor]) > max_tokens
        ):
            refs.append(
                MessageRef(
                    message.id,
                    piece_start,
                    cursor,
                    text[piece_start:cursor],
                )
            )
            piece_start = cursor
        cursor = next_cursor
    if piece_start < end:
        refs.append(MessageRef(message.id, piece_start, end, text[piece_start:end]))
    return refs


def _pack_candidate_refs(
    refs: Sequence[MessageRef],
    max_tokens: int,
) -> list[tuple[MessageRef, ...]]:
    groups: list[tuple[MessageRef, ...]] = []
    current: list[MessageRef] = []
    current_tokens = 0
    for ref in refs:
        ref_tokens = estimate_tokens(ref.text)
        if current and current_tokens + ref_tokens > max_tokens:
            groups.append(tuple(current))
            current = []
            current_tokens = 0
        current.append(ref)
        current_tokens += ref_tokens
    if current:
        groups.append(tuple(current))
    return groups


def _select_context_before(
    refs: Sequence[MessageRef],
    message_limit: int,
) -> tuple[MessageRef, ...]:
    if message_limit == 0:
        return ()
    selected: list[MessageRef] = []
    seen_messages: list[str] = []
    for ref in reversed(refs):
        if ref.message_id not in seen_messages:
            if len(seen_messages) >= message_limit:
                break
            seen_messages.append(ref.message_id)
        selected.append(ref)
    return tuple(reversed(selected))


def _select_context_after(
    refs: Sequence[MessageRef],
    message_limit: int,
) -> tuple[MessageRef, ...]:
    if message_limit == 0:
        return ()
    selected: list[MessageRef] = []
    seen_messages: list[str] = []
    for ref in refs:
        if ref.message_id not in seen_messages:
            if len(seen_messages) >= message_limit:
                break
            seen_messages.append(ref.message_id)
        selected.append(ref)
    return tuple(selected)


def _trim_context_to_budget(
    before: tuple[MessageRef, ...],
    candidate: tuple[MessageRef, ...],
    after: tuple[MessageRef, ...],
    max_tokens: int,
) -> tuple[tuple[MessageRef, ...], tuple[MessageRef, ...]]:
    before_list = list(before)
    after_list = list(after)
    while _refs_tokens((*before_list, *candidate, *after_list)) > max_tokens:
        if before_list:
            before_list.pop(0)
        elif after_list:
            after_list.pop()
        else:
            break
    return tuple(before_list), tuple(after_list)


def _make_window(
    episode: ConversationEpisode,
    candidate: tuple[MessageRef, ...],
    before: tuple[MessageRef, ...],
    after: tuple[MessageRef, ...],
    config: WindowConfig,
) -> ExtractionWindow:
    config_value = asdict(config)
    window_id = "window-" + stable_hash(
        episode.scope_id,
        episode.session_id,
        [ref.to_dict() for ref in candidate],
        [ref.to_dict() for ref in before],
        [ref.to_dict() for ref in after],
        config_value,
    )
    return ExtractionWindow(
        id=window_id,
        scope_id=episode.scope_id,
        session_id=episode.session_id,
        episode_id=episode.id,
        candidate_refs=candidate,
        context_before_refs=before,
        context_after_refs=after,
        candidate_token_count=_refs_tokens(candidate),
        total_token_count=_refs_tokens((*before, *candidate, *after)),
        window_version=config.version,
    )


def _refs_tokens(refs: Sequence[MessageRef]) -> int:
    return sum(estimate_tokens(ref.text) for ref in refs)


def _render_ref(
    ref: MessageRef,
    messages_by_id: Mapping[str, NormalizedMessage],
) -> str:
    message = messages_by_id[ref.message_id]
    header = (
        f"[message_id={message.id} role={message.role} "
        f"speaker={message.speaker or '-'} time={message.timestamp or '-'} "
        f"span={ref.start_char}:{ref.end_char}]"
    )
    return f"{header}\n{ref.text}"
