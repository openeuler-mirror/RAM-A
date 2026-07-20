import argparse
import json
import os
import re
import statistics
import sys
import time
from collections import defaultdict
from pathlib import Path

from dotenv import load_dotenv
from jinja2 import Template
from openai import APIConnectionError, APIStatusError, APITimeoutError, OpenAI
from prompts import ANSWER_PROMPT
from tqdm import tqdm

EVALUATION_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(EVALUATION_ROOT))

from common.memory_pipeline.cache import JsonCache
from locomo.locomo_provenance import query_ref, render_contexts

load_dotenv(".env")

RESULT_PATH_RE = re.compile(
    r"^\$\[(\d+)\]\.conversation\.session_(\d+)\[(\d+)\]\.text$"
)
QUERY_PATH_RE = re.compile(r"^\$\[(\d+)\]\.qa\[(\d+)\]\.question$")
RETRYABLE_STATUS_CODES = {408, 409, 429, 500, 502, 503, 504}
TOKEN_FIELDS = ("prompt_tokens", "completion_tokens", "total_tokens")


def progress_interval(total):
    if total <= 100:
        return 10
    if total <= 1000:
        return 50
    return 100


def should_log_progress(index, total, interval):
    return index == 1 or index == total or index % interval == 0


def elapsed_seconds(started):
    return time.monotonic() - started


def progress_iter(iterable, total, desc):
    if sys.stderr.isatty():
        return tqdm(iterable, total=total, desc=desc)
    return iterable


def token_usage_from_completion(completion):
    usage = getattr(completion, "usage", None)
    if usage is None:
        return {}
    if hasattr(usage, "model_dump"):
        usage = usage.model_dump()
    return {field: usage.get(field) for field in TOKEN_FIELDS if usage.get(field) is not None}


def add_token_usage(record, usage):
    for field in TOKEN_FIELDS:
        record[field] = usage.get(field)


def build_answer_stats(answers):
    by_category = defaultdict(list)
    for items in answers.values():
        for item in items:
            by_category[str(item.get("category", -1))].append(item)

    stats = {}
    for category, items in sorted(by_category.items(), key=lambda value: (0, int(value[0])) if value[0].isdigit() else (1, value[0])):
        answered = [item for item in items if item.get("response_time", 0) > 0]
        latencies = [float(item["response_time"]) for item in answered if item.get("response_time") is not None]
        category_stats = {
            "count": len(items),
            "answered_count": len(answered),
            "skipped_count": len(items) - len(answered),
            "latency_p50_seconds": statistics.median(latencies) if latencies else None,
        }
        for field in TOKEN_FIELDS:
            values = [int(item[field]) for item in answered if item.get(field) is not None]
            category_stats[f"avg_{field}"] = (sum(values) / len(values)) if values else None
        stats[category] = category_stats

    return stats


def print_answer_stats(stats):
    total = sum(int(item.get("count", 0)) for item in stats.values())
    answered = sum(int(item.get("answered_count", 0)) for item in stats.values())
    skipped = total - answered
    print(f"[answer] stats | total={total} answered={answered} skipped={skipped}")


def default_stats_path(output_path):
    return output_path.with_name(f"{output_path.stem}_answer_stats{output_path.suffix}")


class ResponseClient:
    def __init__(self):
        self.client = OpenAI(
            api_key=os.getenv("OPENAI_API_KEY"),
            base_url=os.getenv("OPENAI_BASE_URL") or os.getenv("OPENAI_API_BASE"),
        )
        self.model = os.getenv("MODEL", "gpt-4o-mini")
        self.max_attempts = 8
        self.max_tokens = int(os.getenv("ANSWER_MAX_TOKENS", "512"))

    def answer(self, speaker_1_user_id, speaker_2_user_id, speaker_1_memories, speaker_2_memories, question):
        prompt = Template(ANSWER_PROMPT).render(
            speaker_1_user_id=speaker_1_user_id,
            speaker_2_user_id=speaker_2_user_id,
            speaker_1_memories=json.dumps(speaker_1_memories, indent=4, ensure_ascii=False),
            speaker_2_memories=json.dumps(speaker_2_memories, indent=4, ensure_ascii=False),
            question=question,
        )
        started = time.time()
        for attempt in range(1, self.max_attempts + 1):
            try:
                completion = self.client.chat.completions.create(
                    model=self.model,
                    messages=[{"role": "system", "content": prompt}],
                    temperature=0.0,
                    max_tokens=self.max_tokens,
                )
                return (
                    completion.choices[0].message.content or "",
                    time.time() - started,
                    token_usage_from_completion(completion),
                )
            except (APIConnectionError, APITimeoutError) as exc:
                if attempt == self.max_attempts:
                    raise
                self._retry_sleep(attempt, exc, self.max_attempts)
            except APIStatusError as exc:
                if exc.status_code not in RETRYABLE_STATUS_CODES or attempt == self.max_attempts:
                    raise
                self._retry_sleep(attempt, exc, self.max_attempts)

    @staticmethod
    def _retry_sleep(attempt, exc, max_attempts):
        retry_delay = min(2 ** (attempt - 1), 60)
        print(f"[ANSWER WARN] attempt={attempt} failed: {exc!r}")
        print(f"[ANSWER WARN] retrying in {retry_delay}s ({attempt + 1}/{max_attempts})")
        time.sleep(retry_delay)


class MemoryBenchResponses:
    def __init__(self):
        self.responder = ResponseClient()

    @staticmethod
    def retrieve_context(dataset, sample_index, conversation, results):
        speaker_a = conversation["speaker_a"]
        speaker_b = conversation["speaker_b"]
        contexts = {speaker_a: [], speaker_b: []}
        for result in results:
            path = (result.get("metadata") or {}).get("path", "")
            match = RESULT_PATH_RE.match(path)
            if not match:
                continue
            result_sample, session_number, message_index = (int(value) for value in match.groups())
            if result_sample != sample_index:
                continue
            try:
                source_conversation = dataset[result_sample]["conversation"]
                message = source_conversation[f"session_{session_number}"][message_index]
                timestamp = source_conversation[f"session_{session_number}_date_time"]
            except (IndexError, KeyError, TypeError):
                continue
            memory_text = result.get("text", message.get("text", ""))
            contexts.get(message.get("speaker"), contexts[speaker_a]).append(
                {
                    "memory": f"{message.get('speaker', 'Unknown')}: {memory_text}",
                    "timestamp": timestamp,
                    "score": round(float(result.get("score", 0.0)), 2),
                }
            )
        return contexts

    def answer_question(self, dataset, query_output):
        match = QUERY_PATH_RE.match(query_output.get("query_path", ""))
        if not match:
            raise ValueError(f"Unsupported memory-bench query path: {query_output.get('query_path')!r}")
        sample_index, question_index = (int(value) for value in match.groups())
        question_item = dataset[sample_index]["qa"][question_index]
        conversation = dataset[sample_index]["conversation"]
        speaker_a = conversation["speaker_a"]
        speaker_b = conversation["speaker_b"]
        contexts = self.retrieve_context(dataset, sample_index, conversation, query_output.get("results", []))

        response = ""
        response_time = 0.0
        token_usage = {}
        # Category 5 is adversarial/unanswerable and is excluded from the main
        # LOCOMO QA score. It needs a separate abstention rubric before we ask
        # the answer model to handle it.
        if int(question_item.get("category", -1)) != 5:
            response, response_time, token_usage = self.responder.answer(
                speaker_a,
                speaker_b,
                [f"{m['timestamp']}: {m['memory']}" for m in contexts[speaker_a]],
                [f"{m['timestamp']}: {m['memory']}" for m in contexts[speaker_b]],
                question_item.get("question", ""),
            )

        answer = {
            "question": question_item.get("question", ""),
            "answer": question_item.get("answer", ""),
            "category": question_item.get("category", -1),
            "evidence": question_item.get("evidence", []),
            "response": response,
            "speaker_1_memories": contexts[speaker_a],
            "speaker_2_memories": contexts[speaker_b],
            "num_speaker_1_memories": len(contexts[speaker_a]),
            "num_speaker_2_memories": len(contexts[speaker_b]),
            "speaker_1_graph_memories": None,
            "speaker_2_graph_memories": None,
            "response_time": response_time,
        }
        add_token_usage(answer, token_usage)
        return sample_index, answer

    def generate(self, data_path, search_results):
        with data_path.open("r", encoding="utf-8") as source:
            dataset = json.load(source)

        answers = defaultdict(list)
        total = len(search_results)
        interval = progress_interval(total)
        started = time.monotonic()
        print(f"[answer] started | total={total}")
        iterator = progress_iter(search_results, total, "Answering LoCoMo queries")
        for index, query_output in enumerate(iterator, start=1):
            sample_index, answer = self.answer_question(dataset, query_output)
            answers[str(sample_index)].append(answer)
            if not sys.stderr.isatty() and should_log_progress(index, total, interval):
                print(f"[answer] {index}/{total} done | elapsed={elapsed_seconds(started):.1f}s", flush=True)
        return answers


class PreparedMemoryResponses:
    """Answer LoCoMo queries from prepared raw or extracted memory results."""

    def __init__(self, mode, responder=None, cache=None):
        if mode not in {"raw", "extracted"}:
            raise ValueError(f"unsupported memory mode: {mode}")
        self.mode = mode
        self.responder = responder or ResponseClient()
        self.cache = cache

    def answer_question(self, dataset, prepared_source, query_output):
        ref = query_ref(query_output)
        question = dataset[ref.sample_index]["qa"][ref.question_index]
        conversation = dataset[ref.sample_index]["conversation"]
        speaker_a = conversation["speaker_a"]
        speaker_b = conversation["speaker_b"]
        contexts = render_contexts(
            dataset,
            prepared_source,
            query_output,
            self.mode,
        )
        query_id = query_output["query_id"]
        response = ""
        response_time = 0.0
        token_usage = {}

        if int(question.get("category", -1)) != 5:
            cache_key = [
                "locomo_answer_v1",
                self.mode,
                query_id,
                contexts,
                question.get("question", ""),
                self.responder.model,
                0.0,
            ]
            cached = (
                self.cache.get("locomo-answer", cache_key)
                if self.cache is not None
                else None
            )
            if cached is not None:
                response = str(cached["response"])
                response_time = float(cached["response_time"])
                token_usage = dict(cached["usage"])
            else:
                response, response_time, token_usage = self.responder.answer(
                    speaker_a,
                    speaker_b,
                    [
                        f"{memory['timestamp']}: {memory['memory']}"
                        for memory in contexts[speaker_a]
                    ],
                    [
                        f"{memory['timestamp']}: {memory['memory']}"
                        for memory in contexts[speaker_b]
                    ],
                    question.get("question", ""),
                )
                if self.cache is not None:
                    self.cache.put(
                        "locomo-answer",
                        cache_key,
                        {
                            "response": response,
                            "response_time": response_time,
                            "usage": token_usage,
                        },
                    )

        answer = _answer_record(
            query_id,
            question,
            contexts,
            speaker_a,
            speaker_b,
            response,
            response_time,
            token_usage,
        )
        return ref.sample_index, answer

    def generate(self, dataset, prepared_source, search_results):
        answers = defaultdict(list)
        total = len(search_results)
        interval = progress_interval(total)
        started = time.monotonic()
        print(f"[answer] started | total={total}")
        iterator = progress_iter(search_results, total, "Answering LoCoMo queries")
        for index, query_output in enumerate(iterator, start=1):
            sample_index, answer = self.answer_question(
                dataset,
                prepared_source,
                query_output,
            )
            answers[str(sample_index)].append(answer)
            if not sys.stderr.isatty() and should_log_progress(index, total, interval):
                print(
                    f"[answer] {index}/{total} done | "
                    f"elapsed={elapsed_seconds(started):.1f}s",
                    flush=True,
                )
        return answers


def _answer_record(
    query_id,
    question,
    contexts,
    speaker_a,
    speaker_b,
    response,
    response_time,
    token_usage,
):
    answer = {
        "query_id": query_id,
        "question": question.get("question", ""),
        "answer": question.get("answer", ""),
        "category": question.get("category", -1),
        "evidence": question.get("evidence", []),
        "response": response,
        "speaker_1_memories": contexts[speaker_a],
        "speaker_2_memories": contexts[speaker_b],
        "num_speaker_1_memories": len(contexts[speaker_a]),
        "num_speaker_2_memories": len(contexts[speaker_b]),
        "speaker_1_graph_memories": None,
        "speaker_2_graph_memories": None,
        "response_time": response_time,
    }
    add_token_usage(answer, token_usage)
    return answer


class Mem0Responses:
    def __init__(self):
        self.responder = ResponseClient()

    def generate(self, search_results):
        answers = defaultdict(list)
        total = sum(len(items) for items in search_results.values())
        interval = progress_interval(total)
        started = time.monotonic()
        done = 0
        print(f"[answer] started | total={total}")
        iterator = progress_iter(search_results.items(), len(search_results), "Answering LoCoMo samples")
        for sample_index, items in iterator:
            for item in items:
                response, response_time, token_usage = self.responder.answer(
                    item["speaker_1_user_id"],
                    item["speaker_2_user_id"],
                    [f"{m['timestamp']}: {m['memory']}" for m in item["speaker_1_memories"]],
                    [f"{m['timestamp']}: {m['memory']}" for m in item["speaker_2_memories"]],
                    item.get("question", ""),
                )
                answer = dict(item)
                answer["response"] = response
                answer["response_time"] = response_time
                add_token_usage(answer, token_usage)
                answers[str(sample_index)].append(answer)
                done += 1
                if not sys.stderr.isatty() and should_log_progress(done, total, interval):
                    print(f"[answer] {done}/{total} done | elapsed={elapsed_seconds(started):.1f}s", flush=True)
        return answers


def main():
    parser = argparse.ArgumentParser(description="Generate responses from LoCoMo retrieval results.")
    parser.add_argument(
        "--technique-type",
        choices=("memory_bench", "mem0", "prepared_memory"),
        required=True,
        help="Retrieval result format to process.",
    )
    parser.add_argument("--dataset", type=Path, help="LoCoMo dataset file.")
    parser.add_argument(
        "--prepared-source",
        type=Path,
        help="Raw benchmark-prepared-v1 source used for provenance expansion.",
    )
    parser.add_argument("--memory-mode", choices=("raw", "extracted"))
    parser.add_argument("--cache-dir", type=Path)
    parser.add_argument("--cache-version", default="locomo-answer-v1")
    parser.add_argument(
        "--input",
        type=Path,
        required=True,
        help="Path to the JSON retrieval output generated by the selected technique.",
    )
    parser.add_argument(
        "--output",
        type=Path,
        required=True,
        help="Path to save response records for locomo_eval.py.",
    )
    args = parser.parse_args()

    if args.technique_type in {"memory_bench", "prepared_memory"} and args.dataset is None:
        parser.error(f"--dataset is required when --technique-type {args.technique_type}")
    if args.technique_type == "prepared_memory":
        if args.prepared_source is None or args.memory_mode is None:
            parser.error(
                "--prepared-source and --memory-mode are required when "
                "--technique-type prepared_memory"
            )

    with args.input.open("r", encoding="utf-8") as source:
        search_results = json.load(source)

    if args.technique_type == "mem0":
        answers = Mem0Responses().generate(search_results)
    elif args.technique_type == "prepared_memory":
        dataset = json.loads(args.dataset.read_text(encoding="utf-8"))
        prepared_source = json.loads(
            args.prepared_source.read_text(encoding="utf-8")
        )
        cache = (
            JsonCache(args.cache_dir, version=args.cache_version)
            if args.cache_dir is not None
            else None
        )
        answers = PreparedMemoryResponses(
            args.memory_mode,
            cache=cache,
        ).generate(dataset, prepared_source, search_results)
    elif args.technique_type == "memory_bench":
        answers = MemoryBenchResponses().generate(args.dataset, search_results)
    else:
        raise ValueError(f"Invalid technique type: {args.technique_type}")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("w", encoding="utf-8") as destination:
        json.dump(answers, destination, ensure_ascii=False, indent=4)
    print(f"[answer] wrote responses to {args.output}")

    stats = build_answer_stats(answers)
    stats_path = default_stats_path(args.output)
    with stats_path.open("w", encoding="utf-8") as destination:
        json.dump(stats, destination, ensure_ascii=False, indent=4)
    print(f"[answer] wrote stats to {stats_path}")
    print_answer_stats(stats)


if __name__ == "__main__":
    main()
