from __future__ import annotations

import json

import pytest

from common.memory_pipeline.cache import CacheCorruptionError, JsonCache


def test_json_cache_round_trip_and_versioned_key(tmp_path) -> None:
    cache = JsonCache(tmp_path, version="cache_v1")

    assert cache.get("extraction", ["window", "model", "prompt_v1"]) is None
    path = cache.put(
        "extraction",
        ["window", "model", "prompt_v1"],
        {"memories": []},
    )

    assert path.exists()
    assert cache.get("extraction", ["window", "model", "prompt_v1"]) == {
        "memories": []
    }
    assert cache.get("extraction", ["window", "model", "prompt_v2"]) is None
    assert JsonCache(tmp_path, version="cache_v2").get(
        "extraction", ["window", "model", "prompt_v1"]
    ) is None


def test_cache_rejects_unsafe_namespace(tmp_path) -> None:
    cache = JsonCache(tmp_path)

    with pytest.raises(ValueError, match="unsafe cache namespace"):
        cache.get("../escape", ["key"])


def test_corrupt_cache_is_not_silently_treated_as_miss(tmp_path) -> None:
    cache = JsonCache(tmp_path)
    path = cache.put("verification", ["key"], {"results": []})
    path.write_text("{broken", encoding="utf-8")

    with pytest.raises(CacheCorruptionError, match=str(path)):
        cache.get("verification", ["key"])


def test_cache_file_contains_canonical_json(tmp_path) -> None:
    cache = JsonCache(tmp_path)
    path = cache.put("extraction", ["key"], {"b": 2, "a": 1})

    assert json.loads(path.read_text(encoding="utf-8")) == {"a": 1, "b": 2}
    assert not path.with_suffix(path.suffix + ".tmp").exists()
