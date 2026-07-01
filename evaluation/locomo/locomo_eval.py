import argparse
import concurrent.futures
import json
import math
import os
import re
import sys
import threading
import time
from collections import Counter, defaultdict
from pathlib import Path

from dotenv import load_dotenv
from tqdm import tqdm

EVALUATION_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(EVALUATION_ROOT))

from common.llm_client import OpenAICompatibleClient

try:
    from .prompts import ACCURACY_PROMPT
except ImportError:
    from prompts import ACCURACY_PROMPT

try:
    import nltk
    from nltk.translate.bleu_score import SmoothingFunction, sentence_bleu
except ImportError:
    nltk = None
    SmoothingFunction = None
    sentence_bleu = None

DEFAULT_JUDGE_MODEL = "gpt-4o-mini"
DEFAULT_LLM_API_KEY_ENV = "OPENAI_API_KEY"
DEFAULT_LLM_BASE_URL = "https://api.openai.com/v1"
MAX_JUDGE_ATTEMPTS = 8
ANSWER_STAT_FIELDS = ("prompt_tokens", "completion_tokens", "total_tokens", "response_time")


def progress_interval(total):
    if total <= 100:
        return 10
    if total <= 1000:
        return 50
    return 100


def should_log_progress(index, total, interval):
    return index == 1 or index == total or index % interval == 0

if nltk is not None:
    try:
        nltk.download("punkt", quiet=True)
    except Exception as exc:
        print(f"Error downloading NLTK data: {exc}")


# Adapted from https://github.com/WujiangXu/AgenticMemory/blob/main/utils.py.
def simple_tokenize(text):
    return str(text).lower().replace(".", " ").replace(",", " ").replace("!", " ").replace("?", " ").split()


def calculate_bleu_score(prediction, reference):
    """Calculate BLEU-1, falling back to built-in token overlap if NLTK is unavailable."""
    if nltk is not None:
        try:
            pred_tokens = nltk.word_tokenize(prediction.lower())
            ref_tokens = [nltk.word_tokenize(reference.lower())]
            return sentence_bleu(
                ref_tokens,
                pred_tokens,
                weights=(1, 0, 0, 0),
                smoothing_function=SmoothingFunction().method1,
            )
        except Exception as exc:
            print(f"Falling back to built-in BLEU calculation: {exc}")

    pred_tokens = simple_tokenize(prediction)
    ref_tokens = simple_tokenize(reference)
    if not pred_tokens:
        return 0.0

    brevity_penalty = min(1.0, math.exp(1 - (len(ref_tokens) / len(pred_tokens))))
    predicted = Counter((token,) for token in pred_tokens)
    expected = Counter((token,) for token in ref_tokens)
    overlap = sum((predicted & expected).values())
    precision = overlap / len(pred_tokens) if overlap else 0.1 / len(pred_tokens)
    return brevity_penalty * precision


def calculate_f1_score(prediction, reference):
    if not prediction or not reference:
        return 0.0

    pred_tokens = set(simple_tokenize(str(prediction).strip()))
    ref_tokens = set(simple_tokenize(str(reference).strip()))
    if not pred_tokens or not ref_tokens:
        return 0.0

    common_tokens = pred_tokens & ref_tokens
    precision = len(common_tokens) / len(pred_tokens)
    recall = len(common_tokens) / len(ref_tokens)
    return 2 * precision * recall / (precision + recall) if precision + recall > 0 else 0.0


def _get_response_content(response, model):
    content = getattr(response, "content", None)
    if content:
        return content

    if isinstance(response, str) and response:
        return response

    choices = getattr(response, "choices", None)
    if choices:
        content = getattr(choices[0].message, "content", None)
        if content:
            return content

    details = response.model_dump(exclude_none=True) if hasattr(response, "model_dump") else repr(response)
    raise RuntimeError(f"LLM judge received an empty response for model={model}: {details}")


def _extract_json_object(content):
    candidates = []
    fenced_blocks = re.findall(r"```(?:json)?\s*(\{.*?\})\s*```", content, flags=re.IGNORECASE | re.DOTALL)
    candidates.extend(fenced_blocks)
    candidates.append(content.strip())

    object_start = content.find("{")
    object_end = content.rfind("}")
    if object_start != -1 and object_end != -1 and object_start < object_end:
        candidates.append(content[object_start : object_end + 1])

    for candidate in candidates:
        try:
            parsed = json.loads(candidate)
        except json.JSONDecodeError:
            continue
        if isinstance(parsed, dict):
            return parsed

    raise ValueError(f"LLM judge response does not contain a JSON object: {content!r}")


def _parse_label(content):
    try:
        label = _extract_json_object(content)["label"]
    except (KeyError, TypeError, ValueError):
        labels = re.findall(r"\b(CORRECT|WRONG)\b", content.upper())
        if not labels:
            raise ValueError(f"LLM judge response does not contain a valid label: {content!r}")
        label = labels[-1]

    label = str(label).strip().upper()
    if label not in {"CORRECT", "WRONG"}:
        raise ValueError(f"LLM judge returned an unsupported label: {label!r}")
    return label


def evaluate_llm_judge(
    question,
    gold_answer,
    generated_answer,
    judge_client,
    judge_model,
    max_judge_attempts=MAX_JUDGE_ATTEMPTS,
):
    """Evaluate the generated answer against the gold answer using an LLM judge."""
    messages = [
        {
            "role": "user",
            "content": ACCURACY_PROMPT.format(
                question=question, gold_answer=gold_answer, generated_answer=generated_answer
            ),
        }
    ]

    for attempt in range(1, max_judge_attempts + 1):
        try:
            response = judge_client.chat(
                model=judge_model,
                messages=messages,
                temperature=0.0,
            )
            label = _parse_label(_get_response_content(response, judge_model))
            return 1 if label == "CORRECT" else 0
        except (RuntimeError, ValueError) as exc:
            if attempt == max_judge_attempts:
                raise
            _retry_sleep(attempt, exc, max_judge_attempts)


def _retry_sleep(attempt, exc, max_judge_attempts):
    retry_delay = min(2 ** (attempt - 1), 60)
    print(f"[EVAL WARN] attempt={attempt} failed: {exc!r}")
    print(f"[EVAL WARN] retrying in {retry_delay}s ({attempt + 1}/{max_judge_attempts})")
    time.sleep(retry_delay)


def process_item(item_data, judge_client, judge_model, max_judge_attempts=MAX_JUDGE_ATTEMPTS):
    k, v = item_data
    local_results = defaultdict(list)

    for item in v:
        gt_answer = str(item["answer"])
        pred_answer = str(item["response"])
        category = str(item["category"])
        question = str(item["question"])

        # Category 5 is LoCoMo's adversarial/unanswerable split. Keep it out
        # of the main QA score so results stay comparable with memory-system
        # baselines that exclude no-answer items from LOCOMO accuracy.
        if category == "5":
            continue

        bleu_score = calculate_bleu_score(pred_answer, gt_answer)
        f1_score = calculate_f1_score(pred_answer, gt_answer)
        llm_score = evaluate_llm_judge(
            question,
            gt_answer,
            pred_answer,
            judge_client=judge_client,
            judge_model=judge_model,
            max_judge_attempts=max_judge_attempts,
        )

        result = {
            "question": question,
            "answer": gt_answer,
            "response": pred_answer,
            "category": category,
            "bleu_score": bleu_score,
            "f1_score": f1_score,
            "llm_score": llm_score,
        }
        for field in ANSWER_STAT_FIELDS:
            if field in item:
                result[field] = item[field]
        local_results[k].append(result)

    return local_results


def evaluate_locomo_judge(
    data,
    judge_client,
    judge_model,
    max_workers=10,
    max_judge_attempts=MAX_JUDGE_ATTEMPTS,
    show_progress=True,
):
    if max_workers < 1:
        raise ValueError("--max-workers must be at least 1")

    results = defaultdict(list)
    results_lock = threading.Lock()

    with concurrent.futures.ThreadPoolExecutor(max_workers=max_workers) as executor:
        futures = [
            executor.submit(
                process_item,
                item_data,
                judge_client,
                judge_model,
                max_judge_attempts,
            )
            for item_data in data.items()
        ]
        total = len(futures)
        interval = progress_interval(total)
        started = time.monotonic()
        completed = 0
        correct = 0
        wrong = 0
        if show_progress:
            print(f"[judge] started | groups={total}")
        completed_futures = concurrent.futures.as_completed(futures)
        if show_progress and sys.stderr.isatty():
            completed_futures = tqdm(completed_futures, total=total, desc="Judging LoCoMo samples")
        for future in completed_futures:
            local_results = future.result()
            with results_lock:
                for k, items in local_results.items():
                    results[k].extend(items)
                    for item in items:
                        if int(item.get("llm_score", 0)) == 1:
                            correct += 1
                        else:
                            wrong += 1
            completed += 1
            if show_progress and not sys.stderr.isatty() and should_log_progress(completed, total, interval):
                elapsed = time.monotonic() - started
                print(
                    f"[judge] {completed}/{total} groups done | "
                    f"correct={correct} wrong={wrong} | elapsed={elapsed:.1f}s",
                    flush=True,
                )

    return results


def load_and_evaluate_locomo(
    input_path,
    output_path,
    judge_client,
    judge_model,
    max_workers=10,
    max_judge_attempts=MAX_JUDGE_ATTEMPTS,
    show_progress=True,
):
    with Path(input_path).open("r", encoding="utf-8") as f:
        data = json.load(f)

    results = evaluate_locomo_judge(
        data=data,
        judge_client=judge_client,
        judge_model=judge_model,
        max_workers=max_workers,
        max_judge_attempts=max_judge_attempts,
        show_progress=show_progress,
    )

    output_path = Path(output_path)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    with output_path.open("w", encoding="utf-8") as f:
        json.dump(results, f, indent=4)

    if show_progress:
        print(f"[judge] wrote results to {output_path}")
    return results


def default_llm_base_url():
    return os.getenv("OPENAI_BASE_URL") or os.getenv("OPENAI_API_BASE") or DEFAULT_LLM_BASE_URL


def build_judge_client(llm_api_key_env, llm_base_url, llm_thinking=None):
    return OpenAICompatibleClient(
        api_key_env=llm_api_key_env,
        base_url=llm_base_url,
        thinking=llm_thinking,
    )


def main():
    load_dotenv(".env")

    parser = argparse.ArgumentParser(description="Evaluate RAG results")
    parser.add_argument(
        "--input", type=Path, required=True, help="Path to the input answers file"
    )
    parser.add_argument(
        "--output", type=Path, required=True, help="Path to save the evaluation results"
    )
    parser.add_argument("--judge-model", default=os.getenv("MODEL", DEFAULT_JUDGE_MODEL))
    parser.add_argument("--llm-api-key-env", default=DEFAULT_LLM_API_KEY_ENV)
    parser.add_argument(
        "--llm-base-url",
        default=None,
        help="OpenAI-compatible base URL. Defaults to OPENAI_BASE_URL, OPENAI_API_BASE, or OpenAI.",
    )
    parser.add_argument(
        "--llm-thinking",
        choices=["default", "enabled", "disabled"],
        default="default",
    )
    parser.add_argument("--max-workers", type=int, default=10, help="Maximum number of worker threads")

    args = parser.parse_args()

    judge_client = build_judge_client(
        llm_api_key_env=args.llm_api_key_env,
        llm_base_url=args.llm_base_url or default_llm_base_url(),
        llm_thinking=None if args.llm_thinking == "default" else args.llm_thinking,
    )
    load_and_evaluate_locomo(
        input_path=args.input,
        output_path=args.output,
        judge_client=judge_client,
        judge_model=args.judge_model,
        max_workers=args.max_workers,
    )


if __name__ == "__main__":
    main()
