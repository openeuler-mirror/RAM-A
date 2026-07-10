import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from personalmem.run import write_personamem_run_meta


def test_run_meta_uses_ram_a_backend(tmp_path):
    args = argparse.Namespace(
        run_dir=tmp_path,
        report=tmp_path / "retrieval_metrics.json",
        dataset=tmp_path / "prepared.json",
        output=tmp_path / "search_results.json",
        store=tmp_path / "store.sqlite",
        html_report=None,
        responses=tmp_path / "responses.json",
        grades=tmp_path / "grade_metrics.json",
        top_k=10,
        embedding="hash",
        model="hash",
        dimensions=128,
        search_mode="hybrid",
        candidate_k=None,
        embedding_weight=0.7,
        bm25_weight=0.3,
        store_backend="sqlite",
        answer_model="openai/gpt-4o-mini",
        context_token_budget=2000,
        backend="RAM-A",
    )

    meta = write_personamem_run_meta(args, phase="retrieval")

    assert meta["backend"] == "RAM-A"


def test_run_meta_allows_mem0_backend_override(tmp_path):
    args = argparse.Namespace(
        run_dir=tmp_path,
        report=tmp_path / "retrieval_metrics.json",
        dataset=tmp_path / "prepared.json",
        output=tmp_path / "search_results.json",
        store=tmp_path / "mem0_local",
        html_report=None,
        responses=tmp_path / "responses.json",
        grades=tmp_path / "grade_metrics.json",
        top_k=10,
        embedding="openrouter",
        model="baai/bge-m3",
        dimensions=1024,
        search_mode="hybrid",
        candidate_k=None,
        embedding_weight=0.7,
        bm25_weight=0.3,
        store_backend="sqlite",
        answer_model="openai/gpt-4o-mini",
        context_token_budget=2000,
        backend="mem0",
    )

    meta = write_personamem_run_meta(args, phase="grade")

    assert meta["backend"] == "mem0"
