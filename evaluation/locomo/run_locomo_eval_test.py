import subprocess
from pathlib import Path
import subprocess
import sys

import pytest


EVALUATION_ROOT = Path(__file__).resolve().parents[1]


def test_locomo_source_lives_under_dataset_directory():
    assert (EVALUATION_ROOT / "locomo" / "locomo_report.py").is_file()
    assert not (EVALUATION_ROOT / "scripts" / "locomo").exists()
    assert (EVALUATION_ROOT / "fixtures" / "locomo_sample.json").is_file()
    assert not (EVALUATION_ROOT / "fixtures" / "locomo10.json").exists()
    assert (EVALUATION_ROOT.parent / "data" / "locomo" / "README.md").is_file()


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
    assert 'ANSWER_MODEL="${MODEL:-gpt-4o-mini}"' in content
    assert 'RESUME="${RESUME:-0}"' in content
    assert 'JUDGE_MODEL="${JUDGE_MODEL:-${MODEL:-gpt-4o-mini}}"' in content
    assert 'LLM_API_KEY_ENV="${LLM_API_KEY_ENV:-OPENAI_API_KEY}"' in content
    assert 'RERANK="${RERANK:-0}"' in content
    assert 'RERANK_PROVIDER="${RERANK_PROVIDER:-openrouter}"' in content
    assert 'RERANK_MODEL="${RERANK_MODEL:-cohere/rerank-v3.5}"' in content
    assert 'RERANK_API_KEY_ENV="${RERANK_API_KEY_ENV:-OPENROUTER_API_KEY}"' in content
    assert 'RERANK_BASE_URL="${RERANK_BASE_URL:-https://openrouter.ai/api/v1}"' in content
    assert 'RERANK_INPUT_K="${RERANK_INPUT_K:-40}"' in content
    assert 'MEMORY_BENCH_GRAPH="${MEMORY_BENCH_GRAPH:-0}"' in content
    assert 'MEMORY_BENCH_SEARCH_MODE="${MEMORY_BENCH_SEARCH_MODE:-hybrid}"' in content
    assert 'GRAPH_WEIGHT="${GRAPH_WEIGHT:-0.2}"' in content
    assert 'GRAPH_FAIL_OPEN="${GRAPH_FAIL_OPEN:-0}"' in content
    assert 'GRAPH_MEMORY_SPACE_MODE="${GRAPH_MEMORY_SPACE_MODE:-auto}"' in content
    assert 'GRAPH_MEMORY_SPACE_FIELD="${GRAPH_MEMORY_SPACE_FIELD:-scope_id}"' in content
    assert 'GRAPH_OWNER_ID="${GRAPH_OWNER_ID:-benchmark}"' in content
    assert 'GRAPH_LLM_API_KEY_ENV="${GRAPH_LLM_API_KEY_ENV:-OPENROUTER_API_KEY}"' in content
    assert 'GRAPH_LLM_MODEL="${GRAPH_LLM_MODEL:-openai/gpt-4o-mini}"' in content
    assert 'GRAPH_LLM_BASE_URL="${GRAPH_LLM_BASE_URL:-https://openrouter.ai/api/v1}"' in content
    assert 'GRAPH_LLM_TIMEOUT_MS="${GRAPH_LLM_TIMEOUT_MS:-60000}"' in content
    assert 'GRAPH_BUILD_CONCURRENCY="${GRAPH_BUILD_CONCURRENCY:-1}"' in content
    assert 'MEMORY_BENCH_RERANK_ARGS="' in content
    assert 'MEMORY_BENCH_ADD_RESUME_ARGS="' in content
    assert 'MEMORY_BENCH_GRAPH_ADD_ARGS="' in content
    assert 'MEMORY_BENCH_GRAPH_SEARCH_ARGS="' in content
    assert 'MEMORY_BENCH_GRAPH_ADD_ARGS="--graph-build --graph-build-concurrency $GRAPH_BUILD_CONCURRENCY' in content
    assert '--rerank --rerank-provider $RERANK_PROVIDER' in content
    assert '--rerank-api-key-env $RERANK_API_KEY_ENV' in content
    assert '$MEMORY_BENCH_RERANK_ARGS \\' in content
    assert '$MEMORY_BENCH_GRAPH_ADD_ARGS \\' in content
    assert '--search-mode "$MEMORY_BENCH_SEARCH_MODE" \\' in content
    assert '$MEMORY_BENCH_GRAPH_SEARCH_ARGS \\' in content
    assert 'MEMORY_BENCH_ADD_RESUME_ARGS="--resume"' in content
    assert 'if [ -z "$MEMORY_BENCH_ADD_RESUME_ARGS" ]; then' in content
    assert 'rm -f "$MEMORY_BENCH_STORE"' in content
    assert '--judge-model "$JUDGE_MODEL"' in content
    assert '--llm-api-key-env "$LLM_API_KEY_ENV"' in content
    assert '--llm-base-url "$LLM_BASE_URL"' in content
    assert '--llm-thinking "$LLM_THINKING"' in content
    assert "python3 locomo/locomo_metric.py" in content
    assert "python3 locomo/write_run_meta.py" in content
    assert "python3 locomo/locomo_report.py" in content
    assert "python3 locomo/locomo_experiments.py" in content
    assert "python3 locomo/locomo_retrieval.py" in content
    assert "run_locomo_responses" in content
    assert 'response_api_key="$(printenv "$LLM_API_KEY_ENV" || true)"' in content
    assert 'OPENAI_API_KEY="$response_api_key" OPENAI_BASE_URL="$LLM_BASE_URL" MODEL="$ANSWER_MODEL"' in content
    assert "python3 locomo/locomo_responses.py" in content
    assert "scripts/locomo" not in content
    assert 'MEMORY_BENCH_DIR="${RUN_DIR}/ram-a"' in content
    assert 'MEMORY_BENCH_DATASET="${MEMORY_BENCH_DIR}/prepared.json"' in content
    assert 'python3 locomo/prepare_memory_bench.py \\' in content
    assert '--dataset "$DATASET" \\' in content
    assert '--output "$MEMORY_BENCH_DATASET"' in content
    assert 'write_meta "RAM-A" "all"' in content
    assert 'add --dataset "$MEMORY_BENCH_DATASET" $MEMORY_BENCH_ADD_RESUME_ARGS' in content
    assert '--dataset "$MEMORY_BENCH_DATASET" --top-k "$TOP_K" \\' in content
    assert 'add --dataset "$DATASET" --text-fields text $MEMORY_BENCH_ADD_RESUME_ARGS' not in content
    assert 'search --dataset "$DATASET" --query-fields question --top-k "$TOP_K"' not in content


def test_run_locomo_eval_shell_syntax_is_valid():
    script = EVALUATION_ROOT / "run_locomo_eval.sh"

    subprocess.run(["bash", "-n", str(script)], check=True)


def test_memory_ab_runner_has_paired_modes_and_frozen_config_contract():
    script = EVALUATION_ROOT / "run_locomo_memory_ab.sh"
    content = script.read_text(encoding="utf-8")

    assert 'PHASE="${PHASE:-pilot}"' in content
    assert 'MEMORY_MODE=raw "$PYTHON_BIN" locomo/locomo_run.py' in content
    assert 'MEMORY_MODE=extracted "$PYTHON_BIN" locomo/locomo_run.py' in content
    assert 'locomo/locomo_compare.py' in content
    assert 'OPENROUTER_API_KEY' in content
    assert 'FROZEN_CONFIG' in content
    assert 'locomo/locomo_preflight.py' in content
    assert 'export PREFLIGHT_PATH' in content
    assert '. ./.env' in content


@pytest.mark.parametrize(
    "script",
    (
        "locomo/locomo_compare.py",
        "locomo/locomo_responses.py",
        "locomo/locomo_preflight.py",
    ),
)
def test_memory_ab_python_entrypoints_start_from_evaluation_directory(script):
    completed = subprocess.run(
        [sys.executable, script, "--help"],
        cwd=EVALUATION_ROOT,
        capture_output=True,
        text=True,
        check=False,
    )

    assert completed.returncode == 0, completed.stderr


def test_locomo_readme_documents_graph_mode():
    readme = (EVALUATION_ROOT / "locomo" / "README.md").read_text(encoding="utf-8")

    assert "MEMORY_BENCH_GRAPH=1" in readme
    assert "GRAPH_LLM_MODEL" in readme
    assert "--graph-build" in readme
    assert "--graph" in readme
