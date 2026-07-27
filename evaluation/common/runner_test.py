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
        graph_fail_open=True,
        graph_memory_space_mode="metadata-field",
        graph_memory_space_field="tenant_id",
    )

    cmd = captured["cmd"]
    assert "--graph" in cmd
    assert "--graph-fail-open" in cmd
    assert "--graph-weight" in cmd
    assert "0.4" in cmd
    assert "--graph-memory-space-mode" in cmd
    assert "metadata-field" in cmd
    assert "--graph-memory-space-field" in cmd
    assert "tenant_id" in cmd
    assert cmd.index("--graph") < cmd.index("search")
    assert "--graph-build-concurrency" not in cmd
