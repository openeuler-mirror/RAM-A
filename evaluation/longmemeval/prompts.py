"""Prompt and retrieved-memory formatting helpers for LongMemEval QA."""

from __future__ import annotations

import re


ANSWER_SYSTEM = (
    "You are a personal assistant answering questions from retrieved memories. "
    "Use only the provided memories. If the memories do not contain enough "
    "information, say that the information provided is not enough."
)

ANSWER_PROMPT_VERSION_DEFAULT = "lme_default"
ANSWER_PROMPT_VERSIONS = ("lme_default",)
MEMORY_FORMAT_DEFAULT = "full"
MEMORY_FORMATS = ("full", "compact")

ANSWER_PROMPT_LME_TEMPLATE = """You are answering a LongMemEval personal-memory question.

Question date: {question_date}
Question type: {question_type}
Question: {question}

Retrieved memories:
{memories}

Use the memories as evidence. Follow these rules:
- Answer only from the retrieved memories. Do not use outside knowledge.
- Prefer user messages over assistant suggestions for personal facts.
- For conflicts or updates, use the newest supported fact unless the question asks about an earlier time.
- For time questions, compare event dates and session dates carefully. Words like first, last, before, after, recently, next, previous, ago, and later can change the answer.
- Distinguish planning from completed events. For example, "thinking about buying" is not the same as "bought" or "received".
{extra_rules}
- If multiple memories are needed, combine them before answering.
- If the evidence is insufficient, answer: The information provided is not enough.
- Keep the final answer concise.

Final answer:"""

ANSWER_PROMPT_EXTRA_RULES = ""


def get_judge_prompt(
    question_type: str,
    question_id: str,
    question: str,
    answer: str,
    response: str,
) -> str:
    """Prompt adapted from the official LongMemEval QA judge."""
    if question_id.endswith("_abs"):
        return f"""I will give you an unanswerable question, an explanation, and a model response.
Please answer yes if the model correctly identifies the question as unanswerable.
The model may say that the information is incomplete, or that the asked information is not available.

Question: {question}

Explanation: {answer}

Model Response: {response}

Does the model correctly identify the question as unanswerable? Answer yes or no only."""

    if question_type == "single-session-preference":
        return f"""I will give you a question, a rubric for a desired personalized response, and a model response.
Please answer yes if the response satisfies the desired response. The model does not need to reflect every point in the rubric.
The response is correct as long as it recalls and uses the user's personal information correctly.

Question: {question}

Rubric: {answer}

Model Response: {response}

Is the model response correct? Answer yes or no only."""

    extra = ""
    if question_type == "temporal-reasoning":
        extra = (
            " Do not penalize off-by-one errors for days/weeks/months when "
            "the response otherwise contains the correct answer."
        )
    elif question_type == "knowledge-update":
        extra = (
            " If the response contains previous information as context, it is "
            "still correct as long as the updated answer is clearly the required answer."
        )

    return f"""I will give you a question, a correct answer, and a model response.
Please answer yes if the response contains the correct answer. Otherwise, answer no.
If the response is equivalent to the correct answer or contains all intermediate steps needed to get it, answer yes.
If the response only contains a subset of the required information, answer no.{extra}

Question: {question}

Correct Answer: {answer}

Model Response: {response}

Is the model response correct? Answer yes or no only."""


def format_answer_prompt(
    question: str,
    question_date: str,
    retrieved: list[dict],
    question_type: str = "unknown",
    answer_prompt_version: str = ANSWER_PROMPT_VERSION_DEFAULT,
    memory_format: str = MEMORY_FORMAT_DEFAULT,
    show_scores: bool = False,
) -> str:
    validate_answer_prompt_version(answer_prompt_version)
    validate_memory_format(memory_format)
    memories = format_memories(retrieved, memory_format=memory_format, show_scores=show_scores)
    return ANSWER_PROMPT_LME_TEMPLATE.format(
        question=question,
        question_date=question_date or "(not specified)",
        question_type=question_type or "unknown",
        memories=memories,
        extra_rules=ANSWER_PROMPT_EXTRA_RULES,
    )


def format_memories(
    retrieved: list[dict],
    memory_format: str = MEMORY_FORMAT_DEFAULT,
    show_scores: bool = False,
) -> str:
    validate_memory_format(memory_format)
    if not retrieved:
        return "(No relevant memories found)"
    if memory_format == "compact":
        return _format_memories_compact(retrieved)

    lines = []
    for index, item in enumerate(retrieved, start=1):
        meta = item.get("metadata") or {}
        date = meta.get("session_date") or "unknown date"
        role = meta.get("role") or "unknown"
        memory_id = item.get("id") or "unknown"
        session_id = meta.get("session_id") or "unknown session"
        text = item.get("text", "")
        header = (
            f"{index}. id={memory_id}; session={session_id}; "
            f"date={date}; role={role}"
        )
        if show_scores:
            score = item.get("score", 0.0)
            header += f"; score={score:.4f}"
        lines.append(f"{header}\n{text}")
    return "\n".join(lines)


def validate_answer_prompt_version(answer_prompt_version: str) -> None:
    if answer_prompt_version not in ANSWER_PROMPT_VERSIONS:
        allowed = ", ".join(ANSWER_PROMPT_VERSIONS)
        raise ValueError(
            f"unknown answer prompt version {answer_prompt_version!r}; "
            f"expected one of: {allowed}"
        )


def validate_memory_format(memory_format: str) -> None:
    if memory_format not in MEMORY_FORMATS:
        allowed = ", ".join(MEMORY_FORMATS)
        raise ValueError(
            f"unknown memory format {memory_format!r}; expected one of: {allowed}"
        )


def _format_memories_compact(retrieved: list[dict]) -> str:
    lines = []
    for index, item in enumerate(retrieved, start=1):
        meta = item.get("metadata") or {}
        date = meta.get("session_date") or "unknown date"
        role = meta.get("role") or "unknown"
        text = _normalize_memory_text(item.get("text", ""))
        lines.append(f"[M{index}]\ndate: {date}\nrole: {role}\ncontent: {text}")
    return "\n\n".join(lines)


def _normalize_memory_text(text: str) -> str:
    return re.sub(r"\s+", " ", text).strip()
