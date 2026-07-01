import tempfile
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

from common.backends.base import BackendConfig
from common.backends.factory import create_backend
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


if __name__ == "__main__":
    test_create_rama_backend()
    test_unknown_backend_rejected()
    print("all backend tests passed")
