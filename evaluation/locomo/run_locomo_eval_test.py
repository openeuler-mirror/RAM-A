from pathlib import Path


EVALUATION_ROOT = Path(__file__).resolve().parents[1]


def test_locomo_source_lives_under_dataset_directory():
    assert (EVALUATION_ROOT / "locomo" / "locomo_report.py").is_file()
    assert not (EVALUATION_ROOT / "scripts" / "locomo").exists()
    assert (EVALUATION_ROOT / "fixtures" / "locomo_sample.json").is_file()
    assert not (EVALUATION_ROOT / "fixtures" / "locomo10.json").exists()
    assert (EVALUATION_ROOT.parent / "data" / "locomo" / "locomo10.json").is_file()


def test_locomo_mem0_backend_is_dataset_local():
    assert (EVALUATION_ROOT / "locomo" / "backends" / "mem0" / "add.py").is_file()
    assert (EVALUATION_ROOT / "locomo" / "backends" / "mem0" / "search.py").is_file()
    assert not (EVALUATION_ROOT / "backends" / "mem0" / "add.py").exists()
    assert not (EVALUATION_ROOT / "backends" / "mem0" / "search.py").exists()

    content = (EVALUATION_ROOT / "locomo" / "locomo_experiments.py").read_text(encoding="utf-8")
    assert "from locomo.backends.mem0.add import MemoryADD" in content
    assert "from locomo.backends.mem0.search import MemorySearch" in content
    assert "from backends.mem0" not in content


def test_run_locomo_eval_uses_locomo_entrypoints():
    script = EVALUATION_ROOT / "run_locomo_eval.sh"
    content = script.read_text(encoding="utf-8")

    assert "python3 locomo/locomo_eval.py" in content
    assert 'DATASET="${DATASET:-fixtures/locomo_sample.json}"' in content
    assert 'JUDGE_MODEL="${JUDGE_MODEL:-${MODEL:-gpt-4o-mini}}"' in content
    assert 'LLM_API_KEY_ENV="${LLM_API_KEY_ENV:-OPENAI_API_KEY}"' in content
    assert '--judge-model "$JUDGE_MODEL"' in content
    assert '--llm-api-key-env "$LLM_API_KEY_ENV"' in content
    assert '--llm-base-url "$LLM_BASE_URL"' in content
    assert '--llm-thinking "$LLM_THINKING"' in content
    assert "python3 locomo/locomo_metric.py" in content
    assert "python3 locomo/write_run_meta.py" in content
    assert "python3 locomo/locomo_report.py" in content
    assert "python3 locomo/locomo_experiments.py" in content
    assert "python3 locomo/locomo_retrieval.py" in content
    assert "python3 locomo/locomo_responses.py" in content
    assert "scripts/locomo" not in content
    assert 'MEMORY_BENCH_DIR="${RUN_DIR}/ram-a"' in content
    assert 'write_meta "RAM-A" "all"' in content
