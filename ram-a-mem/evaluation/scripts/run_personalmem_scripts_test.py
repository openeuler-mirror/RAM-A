from pathlib import Path


SCRIPTS_DIR = Path(__file__).resolve().parent


def test_ram_a_personalmem_shell_uses_ram_a_run_dir_artifacts():
    content = (SCRIPTS_DIR / "run_personalmem_ram_a_v1.sh").read_text(encoding="utf-8")

    assert "memory_euler" not in content
    assert "memory-euler" not in content
    assert "run_personalmem_ram_a_v1.sh" in content
    assert 'BACKEND="RAM-A"' in content
    assert 'backend_tag="ram-a"' in content
    assert 'RUN_DIR="outputs/personalmem/personalmem_${SIZE}_v1_${backend_tag}_top${TOP_K}_${context_tag}_${answer_model_tag}"' in content
    assert 'SEARCH_RESULTS="${RUN_DIR}/search_results.json"' in content
    assert 'RETRIEVAL_METRICS="${RUN_DIR}/retrieval_metrics.json"' in content
    assert 'RESPONSES="${RUN_DIR}/responses.json"' in content
    assert 'GRADES="${RUN_DIR}/grade_metrics.json"' in content
    assert 'CSV="${RUN_DIR}/grade_results.csv"' in content
    assert "python3 evaluation/personalmem/run.py eval" in content
    assert '--run-dir "${RUN_DIR}"' in content
    assert '--backend "${BACKEND}"' in content


def test_mem0_personalmem_shell_uses_standard_run_dir_artifacts():
    content = (SCRIPTS_DIR / "run_personalmem_mem0_local_v1.sh").read_text(encoding="utf-8")

    assert 'BACKEND="mem0"' in content
    assert 'backend_tag="mem0"' in content
    assert 'RUN_DIR="outputs/personalmem/personalmem_${SIZE}_v1_${backend_tag}_top${TOP_K}_${context_tag}_${answer_model_tag}"' in content
    assert 'WORK_DIR="${RUN_DIR}/mem0_local"' in content
    assert 'SEARCH_RESULTS="${RUN_DIR}/search_results.json"' in content
    assert 'RETRIEVAL_METRICS="${RUN_DIR}/retrieval_metrics.json"' in content
    assert 'RESPONSES="${RUN_DIR}/responses.json"' in content
    assert 'GRADES="${RUN_DIR}/grade_metrics.json"' in content
    assert 'CSV="${RUN_DIR}/grade_results.csv"' in content
    assert "python3 evaluation/personalmem/run.py eval" in content
    assert '--run-dir "${RUN_DIR}"' in content
    assert '--backend "${BACKEND}"' in content
