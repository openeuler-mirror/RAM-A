import argparse
import json
import re
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


SESSION_KEY_RE = re.compile(r"^session_(\d+)$")


def build_prepared_dataset(raw: list[dict[str, Any]], source: str) -> dict[str, Any]:
    memories: list[dict[str, Any]] = []
    queries: list[dict[str, Any]] = []
    for sample_index, sample in enumerate(raw):
        scope_id = f"path:$[{sample_index}]"
        conversation = sample.get("conversation") or {}
        for session_key in sorted_session_keys(conversation):
            session_number = int(session_key.rsplit("_", 1)[1])
            session_timestamp = conversation.get(f"{session_key}_date_time")
            observed_at_ms = parse_locomo_timestamp_ms(session_timestamp)
            turns = conversation.get(session_key) or []
            for turn_index, turn in enumerate(turns):
                text = str(turn.get("text") or "").strip()
                if not text:
                    continue
                raw_path = f"$[{sample_index}].conversation.{session_key}[{turn_index}].text"
                metadata = {
                    "dataset": "locomo",
                    "scope_id": scope_id,
                    "raw_memory_path": raw_path,
                    "sample_index": sample_index,
                    "session_id": session_key,
                    "session_number": session_number,
                    "turn_index": turn_index,
                }
                copy_optional(metadata, turn, "speaker")
                copy_optional(metadata, turn, "dia_id")
                if sample.get("sample_id") is not None:
                    metadata["sample_id"] = sample["sample_id"]
                if session_timestamp:
                    metadata["session_timestamp"] = session_timestamp
                if observed_at_ms is not None:
                    metadata["observed_at_ms"] = observed_at_ms
                memories.append(
                    {
                        "id": f"{raw_path}:{len(memories)}",
                        "text": text,
                        "metadata": metadata,
                    }
                )

        for question_index, question in enumerate(sample.get("qa") or []):
            text = str(question.get("question") or "").strip()
            if not text:
                continue
            raw_query_path = f"$[{sample_index}].qa[{question_index}].question"
            metadata = {
                "dataset": "locomo",
                "scope_id": scope_id,
                "raw_query_path": raw_query_path,
                "sample_index": sample_index,
                "question_index": question_index,
                "category": question.get("category"),
                "evidence": question.get("evidence", []),
                "answer": question.get("answer", ""),
            }
            if sample.get("sample_id") is not None:
                metadata["sample_id"] = sample["sample_id"]
            target_speaker = infer_target_speaker(
                text,
                [conversation.get("speaker_a"), conversation.get("speaker_b")],
            )
            if target_speaker is not None:
                metadata["target_speaker"] = target_speaker
            queries.append(
                {
                    "id": raw_query_path,
                    "text": text,
                    "filter": {"scope_id": scope_id},
                    "metadata": metadata,
                    "task": {
                        "type": "open_qa",
                        "correct_answer": question.get("answer", ""),
                        "category": question.get("category"),
                        "evidence": question.get("evidence", []),
                    },
                }
            )

    return {
        "schema_version": "benchmark-prepared-v1",
        "dataset": {
            "name": "locomo",
            "source": source,
        },
        "memories": memories,
        "queries": queries,
    }


def sorted_session_keys(conversation: dict[str, Any]) -> list[str]:
    keys = []
    for key, value in conversation.items():
        match = SESSION_KEY_RE.match(key)
        if match and isinstance(value, list):
            keys.append((int(match.group(1)), key))
    return [key for _, key in sorted(keys)]


def copy_optional(target: dict[str, Any], source: dict[str, Any], key: str) -> None:
    if source.get(key) is not None:
        target[key] = source[key]


def infer_target_speaker(question: str, speakers: list[Any]) -> str | None:
    matches: list[tuple[int, str]] = []
    for speaker in speakers:
        if not isinstance(speaker, str) or not speaker.strip():
            continue
        match = re.search(rf"\b{re.escape(speaker)}\b", question, flags=re.IGNORECASE)
        if match:
            matches.append((match.start(), speaker))
    if not matches:
        return None
    return min(matches, key=lambda item: item[0])[1]


def parse_locomo_timestamp_ms(value: Any) -> int | None:
    if not isinstance(value, str) or not value.strip():
        return None
    normalized = re.sub(r"\s+", " ", value.strip())
    for fmt in ("%I:%M %p on %d %B, %Y", "%I:%M %p on %d %b, %Y"):
        try:
            dt = datetime.strptime(normalized.upper(), fmt).replace(tzinfo=timezone.utc)
        except ValueError:
            continue
        return int(dt.timestamp() * 1000)
    return None


def main() -> None:
    parser = argparse.ArgumentParser(description="Convert raw LoCoMo JSON to benchmark-prepared-v1 for memory-bench.")
    parser.add_argument("--dataset", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    raw = json.loads(args.dataset.read_text(encoding="utf-8"))
    if not isinstance(raw, list):
        raise ValueError("LoCoMo dataset must be a JSON array")
    prepared = build_prepared_dataset(raw, source=str(args.dataset))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(prepared, ensure_ascii=False, indent=2), encoding="utf-8")
    print(
        f"wrote LoCoMo prepared memory-bench dataset to {args.output} "
        f"({len(prepared['memories'])} memories, {len(prepared['queries'])} queries)"
    )


if __name__ == "__main__":
    main()
