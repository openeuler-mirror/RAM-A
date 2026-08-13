"""LoCoMo mem0 add pipeline."""

import json
import logging
import os
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
from pathlib import Path

from dotenv import load_dotenv
from mem0.memory.main import Memory

try:
    from tqdm import tqdm
except ImportError:

    def tqdm(iterable, **kwargs):
        return iterable


load_dotenv(".env")


MEM0_EXTRACTION_FAILURE_MARKERS = (
    "LLM extraction failed",
    "Error parsing extraction response",
)


# Update custom instructions
custom_instructions = """
Generate personal memories that follow these guidelines:

1. Each memory should be self-contained with complete context, including:
   - The person's name, do not use "user" while creating memories
   - Personal details (career aspirations, hobbies, life circumstances)
   - Emotional states and reactions
   - Ongoing journeys or future plans
   - Specific dates when events occurred

2. Include meaningful personal narratives focusing on:
   - Identity and self-acceptance journeys
   - Family planning and parenting
   - Creative outlets and hobbies
   - Mental health and self-care activities
   - Career aspirations and education goals
   - Important life events and milestones

3. Make each memory rich with specific details rather than general statements
   - Include timeframes (exact dates when possible)
   - Name specific activities (e.g., "charity race for mental health" rather than just "exercise")
   - Include emotional context and personal growth elements

4. Extract memories only from user messages, not incorporating assistant responses

5. Format each memory as a paragraph with a clear narrative structure that captures the person's
   experience, challenges, and aspirations

6. Return only valid JSON in this form:
   {"memory": [{"id": "0", "text": "...", "attributed_to": "user"}]}
   Return {"memory": []} when no memory is extracted.
"""


class Mem0ExtractionLogCapture(logging.Handler):
    def __init__(self):
        super().__init__(level=logging.ERROR)
        self.thread_id = threading.get_ident()
        self.failures = []

    def emit(self, record):
        if record.thread != self.thread_id:
            return

        message = record.getMessage()
        if any(marker in message for marker in MEM0_EXTRACTION_FAILURE_MARKERS):
            self.failures.append(message)


@contextmanager
def capture_mem0_extraction_failures():
    logger = logging.getLogger("mem0.memory.main")
    handler = Mem0ExtractionLogCapture()
    logger.addHandler(handler)
    try:
        yield handler
    finally:
        logger.removeHandler(handler)


class MemoryADD:
    def __init__(self, data_path, storage_dir, batch_size=2, debug=False, infer=True):
        self.storage_dir = Path(storage_dir)
        self.conversation_storage_dir = self.storage_dir / "conversations"
        embedding_model = os.getenv("EMBEDDING_MODEL", "text-embedding-3-small")
        self.embedding_model = embedding_model
        self.embedding_dims = 1024 if embedding_model.lower() == "baai/bge-m3" else 1536
        self.debug = debug
        self.infer = infer

        self.conversation_storage_dir.mkdir(parents=True, exist_ok=True)

        self.batch_size = batch_size
        self.data_path = data_path
        self.data = None
        if data_path:
            self.load_data()

    def _conversation_storage_path(self, idx):
        return self.conversation_storage_dir / str(idx)

    def _create_mem0(self, storage_dir):
        storage_dir = Path(storage_dir)
        qdrant_path = storage_dir / "qdrant"
        history_db_path = storage_dir / "history.db"

        qdrant_path.mkdir(parents=True, exist_ok=True)
        history_db_path.parent.mkdir(parents=True, exist_ok=True)

        return Memory.from_config(
            {
                "vector_store": {
                    "provider": "qdrant",
                    "config": {
                        "path": str(qdrant_path),
                        "collection_name": f"mem0_eval_local_{self.embedding_dims}",
                        "embedding_model_dims": self.embedding_dims,
                        "on_disk": False,
                    },
                },
                "llm": {
                    "provider": "openai",
                    "config": {"model": os.getenv("MODEL", "gpt-4o-mini")},
                },
                "embedder": {
                    "provider": "openai",
                    "config": {"model": self.embedding_model},
                },
                "history_db_path": str(history_db_path),
                "custom_instructions": custom_instructions,
            }
        )

    def close(self):
        pass

    def _close_mem0(self, mem0):
        if hasattr(mem0, "close"):
            mem0.close()
        vector_store = getattr(mem0, "vector_store", None)
        client = getattr(vector_store, "client", None)
        if hasattr(client, "close"):
            client.close()

    def load_data(self):
        with open(self.data_path, "r") as f:
            self.data = json.load(f)
        return self.data

    def _add_with_infer(self, mem0, user_id, message, metadata):
        timestamp = metadata.get("timestamp")
        prompt = custom_instructions
        if timestamp:
            prompt = f"""{custom_instructions}

Temporal context for this batch:
- Conversation timestamp: {timestamp}
- Use this timestamp as the observation date for all relative time expressions
  such as "yesterday", "today", "last week", "last month", and "recently".
- Never use the system current date to resolve dates in these messages.
- Write resolved relative dates explicitly; if unsure, keep the relative wording
  and include the conversation timestamp instead of inventing a date.
"""

        with capture_mem0_extraction_failures() as extraction_logs:
            result = mem0.add(
                message,
                user_id=user_id,
                metadata=metadata,
                infer=True,
                prompt=prompt,
            )
        return result, extraction_logs.failures

    def _add_without_infer(self, mem0, user_id, message, metadata):
        return mem0.add(
            message,
            user_id=user_id,
            metadata=metadata,
            infer=False,
        )

    def add_memory(self, mem0, user_id, message, metadata, retries=8):
        for attempt in range(retries):
            try:
                if self.debug:
                    print(f"\n[ADD DEBUG] user_id={user_id}, timestamp={metadata.get('timestamp')}")
                    print("[ADD DEBUG] input:")
                    print(json.dumps(message, indent=2, ensure_ascii=False))

                if self.infer:
                    result, extraction_failures = self._add_with_infer(mem0, user_id, message, metadata)
                else:
                    result = self._add_without_infer(mem0, user_id, message, metadata)
                    extraction_failures = []

                if self.debug:
                    print("[ADD DEBUG] result:")
                    print(json.dumps(result, indent=2, ensure_ascii=False, default=str))

                if not extraction_failures:
                    return result

                print(f"[ADD WARN] AI memory extraction failed attempt={attempt + 1}: {extraction_failures[-1]}")
                if attempt < retries - 1:
                    retry_delay = min(2**attempt, 60)
                    print(f"[ADD WARN] retrying in {retry_delay}s ({attempt + 2}/{retries})")
                    time.sleep(retry_delay)
                    continue

                print("[ADD WARN] falling back to raw message storage with infer=False")
                fallback_result = self._add_without_infer(mem0, user_id, message, metadata)
                if self.debug:
                    print("[ADD DEBUG] fallback result:")
                    print(json.dumps(fallback_result, indent=2, ensure_ascii=False, default=str))
                return fallback_result
            except Exception as exc:
                print(f"[ADD DEBUG] error attempt={attempt + 1}: {exc!r}")
                if attempt < retries - 1:
                    retry_delay = min(2**attempt, 60)
                    print(f"[ADD WARN] retrying in {retry_delay}s ({attempt + 2}/{retries})")
                    time.sleep(retry_delay)
                    continue
                raise

    def add_memories_for_speaker(self, mem0, speaker, messages, timestamp, desc):
        for i in tqdm(range(0, len(messages), self.batch_size), desc=desc, leave=False):
            batch_messages = messages[i : i + self.batch_size]
            self.add_memory(mem0, speaker, batch_messages, metadata={"timestamp": timestamp})

    def process_conversation(self, item, idx):
        conversation = item["conversation"]
        speaker_a = conversation["speaker_a"]
        speaker_b = conversation["speaker_b"]

        speaker_a_user_id = f"{speaker_a}_{idx}"
        speaker_b_user_id = f"{speaker_b}_{idx}"

        mem0 = self._create_mem0(self._conversation_storage_path(idx))
        try:
            # delete all memories for the two users in this conversation shard
            mem0.delete_all(user_id=speaker_a_user_id)
            mem0.delete_all(user_id=speaker_b_user_id)

            for key in conversation.keys():
                if key in ["speaker_a", "speaker_b"] or "date" in key or "timestamp" in key:
                    continue

                date_time_key = key + "_date_time"
                timestamp = conversation[date_time_key]
                chats = conversation[key]
                if self.debug:
                    print(f"\n[ADD DEBUG] session={key}, timestamp={timestamp}")

                messages = []
                messages_reverse = []
                for chat in chats:
                    if chat["speaker"] == speaker_a:
                        messages.append({"role": "user", "content": f"{speaker_a}: {chat['text']}"})
                        messages_reverse.append({"role": "assistant", "content": f"{speaker_a}: {chat['text']}"})
                    elif chat["speaker"] == speaker_b:
                        messages.append({"role": "assistant", "content": f"{speaker_b}: {chat['text']}"})
                        messages_reverse.append({"role": "user", "content": f"{speaker_b}: {chat['text']}"})
                    else:
                        raise ValueError(f"Unknown speaker: {chat['speaker']}")

                self.add_memories_for_speaker(
                    mem0,
                    speaker_a_user_id,
                    messages,
                    timestamp,
                    f"Adding memories for {speaker_a_user_id}",
                )
                self.add_memories_for_speaker(
                    mem0,
                    speaker_b_user_id,
                    messages_reverse,
                    timestamp,
                    f"Adding memories for {speaker_b_user_id}",
                )

            print(f"Conversation {idx} added successfully")
        finally:
            self._close_mem0(mem0)

    def process_all_conversations(self, max_workers=4):
        if not self.data:
            raise ValueError("No data loaded. Please set data_path and call load_data() first.")

        with ThreadPoolExecutor(max_workers=max_workers) as executor:
            futures = [executor.submit(self.process_conversation, item, idx) for idx, item in enumerate(self.data)]

            for future in futures:
                future.result()
