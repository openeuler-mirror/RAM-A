import subprocess

from common import runner


def test_run_add_passes_graph_build_flags(monkeypatch, tmp_path):
    captured = {}

    def fake_run(cmd, capture_output, cwd):
        captured["cmd"] = cmd
        captured["cwd"] = cwd
        return subprocess.CompletedProcess(cmd, 0)

    monkeypatch.setattr(runner.subprocess, "run", fake_run)

    runner.run_add(
        tmp_path / "store.sqlite",
        tmp_path / "prepared.json",
        graph_build=True,
        graph_build_concurrency=4,
        resume=True,
        graph_weight=0.4,
        graph_memory_space_mode="metadata-field",
        graph_memory_space_field="tenant_id",
        graph_owner_id="bench-owner",
        graph_llm_api_key_env="GRAPH_KEY",
        graph_llm_model="openai/gpt-4o-mini",
        graph_llm_base_url="https://openrouter.ai/api/v1",
        graph_llm_timeout_ms=60000,
    )

    cmd = captured["cmd"]
    assert "--graph-build" in cmd
    assert cmd[cmd.index("--graph-build-concurrency") + 1] == "4"
    assert "--graph-weight" in cmd
    assert "0.4" in cmd
    assert "--graph-memory-space-mode" in cmd
    assert "metadata-field" in cmd
    assert "--graph-memory-space-field" in cmd
    assert "tenant_id" in cmd
    assert "--graph-owner-id" in cmd
    assert "bench-owner" in cmd
    assert "--graph-llm-api-key-env" in cmd
    assert "GRAPH_KEY" in cmd
    assert "--graph-llm-timeout-ms" in cmd
    assert "60000" in cmd
    assert cmd.index("--graph-build") < cmd.index("add")
    assert cmd.index("--graph-build-concurrency") < cmd.index("add")
    assert "--resume" in cmd[cmd.index("add"):]


def test_run_search_passes_graph_flags(monkeypatch, tmp_path):
    captured = {}

    def fake_run(cmd, capture_output, cwd):
        captured["cmd"] = cmd
        captured["cwd"] = cwd
        return subprocess.CompletedProcess(cmd, 0)

    monkeypatch.setattr(runner.subprocess, "run", fake_run)

    runner.run_search(
        tmp_path / "store.sqlite",
        tmp_path / "prepared.json",
        tmp_path / "search.json",
        graph=True,
        graph_weight=0.4,
        graph_rerank=True,
        graph_allow_graph_only=True,
        graph_max_graph_only_results=6,
        graph_fail_open=True,
        graph_memory_space_mode="metadata-field",
        graph_memory_space_field="tenant_id",
    )

    cmd = captured["cmd"]
    assert "--graph" in cmd
    assert "--graph-fail-open" in cmd
    assert "--graph-rerank" in cmd
    assert "--graph-allow-graph-only" in cmd
    assert cmd[cmd.index("--graph-max-graph-only-results") + 1] == "6"
    assert "--graph-weight" in cmd
    assert "0.4" in cmd
    assert "--graph-memory-space-mode" in cmd
    assert "metadata-field" in cmd
    assert "--graph-memory-space-field" in cmd
    assert "tenant_id" in cmd
    assert cmd.index("--graph") < cmd.index("search")
    assert "--graph-build-concurrency" not in cmd


def test_run_search_default_off_omits_rerank_flags(monkeypatch, tmp_path):
    captured = {}

    def fake_run(cmd, capture_output, cwd):
        captured["cmd"] = cmd
        return subprocess.CompletedProcess(cmd, 0)

    monkeypatch.setattr(runner.subprocess, "run", fake_run)

    store_path = tmp_path / "store.sqlite"
    dataset_path = tmp_path / "prepared.json"
    output_path = tmp_path / "search.json"
    runner.run_search(store_path, dataset_path, output_path)

    assert captured["cmd"] == runner.CARGO_BIN + [
        "--store", str(store_path),
        "--embedding", "hash",
        "--model", runner.DEFAULT_EMBEDDING_MODEL,
        "--dimensions", str(runner.DEFAULT_DIMENSIONS),
        "--api-key-env", runner.DEFAULT_API_KEY_ENV,
        "--batch-size", "64",
        "search",
        "--dataset", str(dataset_path),
        "--output", str(output_path),
        "--top-k", "10",
    ]


def test_run_search_enabled_passes_all_rerank_flags_before_search(monkeypatch, tmp_path):
    captured = {}

    def fake_run(cmd, capture_output, cwd):
        captured["cmd"] = cmd
        return subprocess.CompletedProcess(cmd, 0)

    monkeypatch.setattr(runner.subprocess, "run", fake_run)

    runner.run_search(
        tmp_path / "store.sqlite",
        tmp_path / "prepared.json",
        tmp_path / "search.json",
        rerank=True,
        rerank_provider="openrouter",
        rerank_model="cohere/rerank-v3.5",
        rerank_api_key_env="RERANK_KEY",
        rerank_base_url="https://rerank.example/v1",
        rerank_input_k=80,
        rerank_timeout_ms=30000,
        rerank_fail_open=True,
    )

    cmd = captured["cmd"]
    expected = [
        "--rerank",
        "--rerank-provider", "openrouter",
        "--rerank-model", "cohere/rerank-v3.5",
        "--rerank-api-key-env", "RERANK_KEY",
        "--rerank-base-url", "https://rerank.example/v1",
        "--rerank-input-k", "80",
        "--rerank-timeout-ms", "30000",
        "--rerank-fail-open",
    ]
    assert [argument for argument in cmd if argument.startswith("--rerank")] == [
        "--rerank",
        "--rerank-provider",
        "--rerank-model",
        "--rerank-api-key-env",
        "--rerank-base-url",
        "--rerank-input-k",
        "--rerank-timeout-ms",
        "--rerank-fail-open",
    ]
    for index, argument in enumerate(expected):
        assert cmd[cmd.index("--rerank") + index] == argument
    assert cmd.index("--rerank") < cmd.index("search")


def test_run_search_enabled_omits_unset_optional_rerank_flags(monkeypatch, tmp_path):
    captured = {}

    def fake_run(cmd, capture_output, cwd):
        captured["cmd"] = cmd
        return subprocess.CompletedProcess(cmd, 0)

    monkeypatch.setattr(runner.subprocess, "run", fake_run)

    runner.run_search(
        tmp_path / "store.sqlite",
        tmp_path / "prepared.json",
        tmp_path / "search.json",
        rerank=True,
        rerank_timeout_ms=None,
        rerank_fail_open=False,
    )

    cmd = captured["cmd"]
    assert "--rerank" in cmd
    assert cmd[cmd.index("--rerank-provider") + 1] == "openrouter"
    assert cmd[cmd.index("--rerank-model") + 1] == "cohere/rerank-v3.5"
    assert "--rerank-timeout-ms" not in cmd
    assert "--rerank-fail-open" not in cmd
