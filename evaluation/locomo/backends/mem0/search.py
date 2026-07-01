"""LoCoMo mem0 search pipeline."""

import json
import os
import time
from collections import defaultdict
from pathlib import Path

from dotenv import load_dotenv
from mem0.memory.main import Memory

try:
    from tqdm import tqdm
except ImportError:

    def tqdm(iterable, **kwargs):
        return iterable


load_dotenv(".env")


class MemorySearch:
    def __init__(self, output_path, storage_dir, top_k=10, threshold=0.1):
        self.storage_dir = Path(storage_dir)
        self.conversation_storage_dir = self.storage_dir / "conversations"
        self.use_conversation_shards = self.conversation_storage_dir.exists()
        embedding_model = os.getenv("EMBEDDING_MODEL", "text-embedding-3-small")
        self.embedding_model = embedding_model
        self.embedding_dims = 1024 if embedding_model.lower() == "baai/bge-m3" else 1536

        if self.use_conversation_shards:
            self.mem0 = None
        else:
            self.mem0 = self._create_mem0(self.storage_dir)
        self.top_k = top_k
        self.threshold = threshold
        self.results = defaultdict(list)
        self.output_path = Path(output_path)
        self.output_path.parent.mkdir(parents=True, exist_ok=True)

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
            }
        )

    def close(self):
        if self.mem0 is not None:
            self._close_mem0(self.mem0)

    def _close_mem0(self, mem0):
        if hasattr(mem0, "close"):
            mem0.close()
        vector_store = getattr(mem0, "vector_store", None)
        client = getattr(vector_store, "client", None)
        if hasattr(client, "close"):
            client.close()

    def search_memory(self, mem0, user_id, query, max_retries=3, retry_delay=1):
        start_time = time.time()
        for attempt in range(max_retries):
            try:
                memories = mem0.search(
                    query,
                    top_k=self.top_k,
                    filters={"user_id": user_id},
                    threshold=self.threshold,
                )["results"]
                break
            except Exception:
                if attempt >= max_retries - 1:
                    raise
                print("Retrying local memory search...")
                time.sleep(retry_delay)

        semantic_memories = [
            {
                "memory": memory["memory"],
                "timestamp": (memory.get("metadata") or {}).get("timestamp"),
                "score": round(memory.get("score") or 0, 2),
            }
            for memory in memories
        ]
        graph_memories = None
        return semantic_memories, graph_memories, time.time() - start_time

    def retrieve_question(self, mem0, speaker_1_user_id, speaker_2_user_id, question):
        speaker_1_memories, speaker_1_graph_memories, speaker_1_memory_time = self.search_memory(
            mem0, speaker_1_user_id, question
        )
        speaker_2_memories, speaker_2_graph_memories, speaker_2_memory_time = self.search_memory(
            mem0, speaker_2_user_id, question
        )
        return (
            speaker_1_memories,
            speaker_2_memories,
            speaker_1_memory_time,
            speaker_2_memory_time,
            speaker_1_graph_memories,
            speaker_2_graph_memories,
        )

    def process_question(self, mem0, val, speaker_a, speaker_b, speaker_a_user_id, speaker_b_user_id):
        question = val.get("question", "")
        answer = val.get("answer", "")
        category = val.get("category", -1)
        evidence = val.get("evidence", [])
        adversarial_answer = val.get("adversarial_answer", "")

        (
            speaker_1_memories,
            speaker_2_memories,
            speaker_1_memory_time,
            speaker_2_memory_time,
            speaker_1_graph_memories,
            speaker_2_graph_memories,
        ) = self.retrieve_question(mem0, speaker_a_user_id, speaker_b_user_id, question)

        return {
            "question": question,
            "answer": answer,
            "category": category,
            "evidence": evidence,
            "adversarial_answer": adversarial_answer,
            "speaker_1_user_id": speaker_a,
            "speaker_2_user_id": speaker_b,
            "speaker_1_memories": speaker_1_memories,
            "speaker_2_memories": speaker_2_memories,
            "num_speaker_1_memories": len(speaker_1_memories),
            "num_speaker_2_memories": len(speaker_2_memories),
            "speaker_1_memory_time": speaker_1_memory_time,
            "speaker_2_memory_time": speaker_2_memory_time,
            "speaker_1_graph_memories": speaker_1_graph_memories,
            "speaker_2_graph_memories": speaker_2_graph_memories,
        }

    def process_data_file(self, file_path):
        with open(file_path, "r") as f:
            data = json.load(f)

        for idx, item in tqdm(enumerate(data), total=len(data), desc="Processing conversations"):
            qa = item["qa"]
            conversation = item["conversation"]
            speaker_a = conversation["speaker_a"]
            speaker_b = conversation["speaker_b"]

            speaker_a_user_id = f"{speaker_a}_{idx}"
            speaker_b_user_id = f"{speaker_b}_{idx}"

            mem0 = self._create_mem0(self._conversation_storage_path(idx)) if self.use_conversation_shards else self.mem0
            try:
                for question_item in tqdm(
                    qa,
                    total=len(qa),
                    desc=f"Processing questions for conversation {idx}",
                    leave=False,
                ):
                    result = self.process_question(
                        mem0,
                        question_item,
                        speaker_a,
                        speaker_b,
                        speaker_a_user_id,
                        speaker_b_user_id,
                    )
                    self.results[idx].append(result)

                    with open(self.output_path, "w") as f:
                        json.dump(self.results, f, indent=4)
            finally:
                if self.use_conversation_shards:
                    self._close_mem0(mem0)

        with open(self.output_path, "w") as f:
            json.dump(self.results, f, indent=4)
