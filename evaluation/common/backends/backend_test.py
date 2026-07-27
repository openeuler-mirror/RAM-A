import tempfile
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

from common.backends.base import BackendConfig
from common.backends.factory import create_backend
from common.backends import rama as rama_module
from common.backends.rama import RamaBackend


def _config(name: str = "RAM-A") -> BackendConfig:
    return BackendConfig(
        name=name,
        store_path=Path(tempfile.gettempdir()) / "backend-test-store.jsonl",
        embedding="hash",
        embedding_model="hash",
        dimensions=128,
        api_key_env="OPENROUTER_API_KEY",
        batch_size=2,
        top_k=3,
    )


def test_create_rama_backend():
    backend = create_backend(_config())
    assert isinstance(backend, RamaBackend)


def test_unknown_backend_rejected():
    try:
        create_backend(_config("unknown"))
    except ValueError as error:
        assert "unknown backend" in str(error)
    else:
        raise AssertionError("unknown backend should fail")


def test_backend_config_carries_graph_options():
    config = _config()

    assert config.graph is False
    assert config.graph_build is False
    assert config.graph_build_concurrency == 1
    assert config.resume is False
    assert config.graph_weight == 0.2
    assert config.graph_fail_open is False
    assert config.graph_memory_space_mode == "auto"
    assert config.graph_memory_space_field == "scope_id"
    assert config.graph_owner_id == "benchmark"
    assert config.graph_llm_api_key_env == "OPENROUTER_API_KEY"
    assert config.graph_llm_model == "openai/gpt-4o-mini"
    assert config.graph_llm_base_url == "https://openrouter.ai/api/v1"
    assert config.graph_llm_timeout_ms is None


def test_rama_backend_passes_graph_options(monkeypatch, tmp_path):
    captured = {}

    def fake_run_add(*args, **kwargs):
        captured["add"] = kwargs

    def fake_run_search(*args, **kwargs):
        captured["search"] = kwargs

    monkeypatch.setattr(rama_module, "run_add", fake_run_add)
    monkeypatch.setattr(rama_module, "run_search", fake_run_search)

    config = BackendConfig(
        name="RAM-A",
        store_path=tmp_path / "store.sqlite",
        embedding="hash",
        embedding_model="hash",
        dimensions=128,
        api_key_env="OPENROUTER_API_KEY",
        batch_size=2,
        top_k=3,
        graph=True,
        graph_build=True,
        graph_build_concurrency=4,
        resume=True,
        graph_weight=0.4,
        graph_fail_open=True,
        graph_memory_space_mode="metadata-field",
        graph_memory_space_field="tenant_id",
        graph_owner_id="bench-owner",
        graph_llm_api_key_env="GRAPH_KEY",
        graph_llm_model="openai/gpt-4o-mini",
        graph_llm_base_url="https://openrouter.ai/api/v1",
        graph_llm_timeout_ms=60000,
    )

    backend = RamaBackend(config)
    backend.add(tmp_path / "prepared.json")
    backend.search(tmp_path / "prepared.json", tmp_path / "search.json")

    assert captured["add"]["graph_build"] is True
    assert captured["add"]["graph_build_concurrency"] == 4
    assert captured["add"]["resume"] is True
    assert captured["add"]["graph_weight"] == 0.4
    assert captured["add"]["graph_memory_space_mode"] == "metadata-field"
    assert captured["add"]["graph_llm_api_key_env"] == "GRAPH_KEY"
    assert captured["search"]["graph"] is True
    assert "graph_build_concurrency" not in captured["search"]
    assert captured["search"]["graph_fail_open"] is True
    assert captured["search"]["graph_memory_space_field"] == "tenant_id"


if __name__ == "__main__":
    test_create_rama_backend()
    test_unknown_backend_rejected()
    print("all backend tests passed")
