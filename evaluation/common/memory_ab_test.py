from __future__ import annotations

import hashlib
import json
from pathlib import Path

import pytest

from common.memory_ab import (
    ExperimentArmConfig,
    canonical_sha256,
    ensure_run_mode,
    ensure_store_mode,
    file_sha256,
    validate_frozen_manifest,
    validate_memory_ab_preflight,
    validate_pair_contract,
)


def _config(memory_mode: str, run_dir: Path) -> dict[str, object]:
    return ExperimentArmConfig(
        dataset="locomo",
        phase="pilot",
        memory_mode=memory_mode,
        source_path=run_dir / "source.json",
        run_dir=run_dir,
        immutable={
            "source_hash": "source-sha256",
            "implementation_hash": "implementation-sha256",
            "preflight_hash": "preflight-sha256",
        },
    ).public_manifest()


def test_experiment_arm_manifest_hashes_only_immutable_configuration(tmp_path):
    config = ExperimentArmConfig(
        dataset="locomo",
        phase="pilot",
        memory_mode="raw",
        source_path=tmp_path / "locomo.json",
        run_dir=tmp_path / "raw",
        immutable={"top_k": 30, "model": "baai/bge-m3"},
    )

    manifest = config.public_manifest()

    assert manifest == {
        "dataset": "locomo",
        "phase": "pilot",
        "memory_mode": "raw",
        "source_path": str(tmp_path / "locomo.json"),
        "run_dir": str(tmp_path / "raw"),
        "top_k": 30,
        "model": "baai/bge-m3",
        "configuration_hash": canonical_sha256(config.immutable),
    }


def test_canonical_sha256_is_stable_across_mapping_order():
    first = {"model": "bge-m3", "weights": [0.7, 0.3]}
    second = {"weights": [0.7, 0.3], "model": "bge-m3"}

    assert canonical_sha256(first) == canonical_sha256(second)


def test_file_sha256_hashes_file_bytes(tmp_path):
    source = tmp_path / "source.json"
    source.write_bytes(b'{"memory":"raw"}\n')

    assert file_sha256(source) == hashlib.sha256(source.read_bytes()).hexdigest()


def test_pair_contract_allows_only_memory_mode_and_paths_to_differ(tmp_path):
    raw = _config("raw", tmp_path / "raw")
    extracted = _config("extracted", tmp_path / "extracted")
    prepared = {"schema_version": "benchmark-prepared-v1", "queries": [{"id": "q1"}]}
    contract = validate_pair_contract(raw, extracted, prepared, prepared)
    assert contract["query_count"] == 1
    assert contract["configuration_hash"] == raw["configuration_hash"]


def test_pair_contract_rejects_query_order_change(tmp_path):
    raw_prepared = {"queries": [{"id": "q1"}, {"id": "q2"}]}
    extracted_prepared = {"queries": [{"id": "q2"}, {"id": "q1"}]}
    with pytest.raises(ValueError, match="prepared queries differ"):
        validate_pair_contract(
            _config("raw", tmp_path / "r"),
            _config("extracted", tmp_path / "e"),
            raw_prepared,
            extracted_prepared,
        )


@pytest.mark.parametrize(
    ("raw_mode", "extracted_mode"),
    [("extracted", "raw"), ("raw", "raw")],
)
def test_pair_contract_requires_ordered_raw_and_extracted_arms(
    tmp_path, raw_mode, extracted_mode
):
    with pytest.raises(ValueError, match="must declare raw and extracted"):
        validate_pair_contract(
            _config(raw_mode, tmp_path / "r"),
            _config(extracted_mode, tmp_path / "e"),
            {"queries": []},
            {"queries": []},
        )


@pytest.mark.parametrize(
    "field",
    ["source_hash", "configuration_hash", "implementation_hash", "preflight_hash"],
)
def test_pair_contract_requires_matching_provenance(tmp_path, field):
    raw = _config("raw", tmp_path / "r")
    extracted = _config("extracted", tmp_path / "e")
    extracted[field] = "different"

    with pytest.raises(ValueError, match=rf"raw/extracted {field} mismatch"):
        validate_pair_contract(raw, extracted, {"queries": []}, {"queries": []})


@pytest.mark.parametrize(
    "queries",
    [[{"id": ""}], [{"id": "q1"}, {"id": "q1"}]],
)
def test_pair_contract_rejects_missing_or_duplicated_query_ids(tmp_path, queries):
    with pytest.raises(ValueError, match="ids are missing or duplicated"):
        validate_pair_contract(
            _config("raw", tmp_path / "r"),
            _config("extracted", tmp_path / "e"),
            {"queries": queries},
            {"queries": queries},
        )


def test_frozen_manifest_compares_only_current_immutable_fields(tmp_path):
    frozen = tmp_path / "frozen.json"
    frozen.write_text(
        json.dumps({"top_k": 30, "model": "bge-m3", "phase": "pilot"}),
        encoding="utf-8",
    )

    validate_frozen_manifest({"top_k": 30, "model": "bge-m3"}, frozen)


def test_frozen_manifest_rejects_changed_immutable_field(tmp_path):
    frozen = tmp_path / "frozen.json"
    frozen.write_text(json.dumps({"top_k": 29, "model": "bge-m3"}), encoding="utf-8")
    with pytest.raises(
        ValueError, match="frozen configuration mismatch for fields: top_k"
    ):
        validate_frozen_manifest({"top_k": 30, "model": "bge-m3"}, frozen)


def test_run_directory_rejects_switching_memory_mode(tmp_path):
    ensure_run_mode(tmp_path, "raw")
    ensure_run_mode(tmp_path, "raw")

    with pytest.raises(ValueError, match="already belongs to memory mode raw"):
        ensure_run_mode(tmp_path, "extracted")


def test_existing_unmarked_store_rejects_extracted_claim_without_mutation(tmp_path):
    store = tmp_path / "legacy.sqlite"
    original = b"legacy raw store\n"
    store.write_bytes(original)
    marker = Path(f"{store.resolve()}.memory_mode")

    with pytest.raises(
        ValueError,
        match="existing unowned store.*new store.*raw migration",
    ):
        ensure_store_mode(store, "extracted")

    assert store.read_bytes() == original
    assert not marker.exists()


def test_existing_unmarked_store_allows_explicit_raw_migration(tmp_path):
    store = tmp_path / "legacy.sqlite"
    original = b"legacy raw store\n"
    store.write_bytes(original)

    ensure_store_mode(store, "raw")

    assert store.read_bytes() == original
    assert (
        Path(f"{store.resolve()}.memory_mode").read_text(encoding="utf-8")
        == "raw\n"
    )


def test_new_store_allows_extracted_claim(tmp_path):
    store = tmp_path / "new.sqlite"

    ensure_store_mode(store, "extracted")

    assert not store.exists()
    assert (
        Path(f"{store.resolve()}.memory_mode").read_text(encoding="utf-8")
        == "extracted\n"
    )


def test_memory_ab_preflight_returns_hash_for_complete_matching_report(tmp_path):
    report = {
        "schema_version": "memory-ab-preflight-v1",
        "dataset": "personalmem",
        "implementation_hash": "implementation-sha",
        "passed": True,
        "suites": [
            {"name": name, "exit_code": 0}
            for name in (
                "python_evaluation",
                "rust_workspace",
                "rust_clippy",
                "diff_check",
            )
        ],
    }
    path = tmp_path / "preflight.json"
    path.write_text(json.dumps(report), encoding="utf-8")

    assert validate_memory_ab_preflight(
        path, "personalmem", "implementation-sha"
    ) == file_sha256(path)


def test_memory_ab_preflight_rejects_wrong_implementation_before_arm_work(
    tmp_path,
):
    report = {
        "schema_version": "memory-ab-preflight-v1",
        "dataset": "longmemeval",
        "implementation_hash": "old-code",
        "passed": True,
        "suites": [
            {"name": name, "exit_code": 0}
            for name in (
                "python_evaluation",
                "rust_workspace",
                "rust_clippy",
                "diff_check",
            )
        ],
    }
    path = tmp_path / "preflight.json"
    path.write_text(json.dumps(report), encoding="utf-8")

    with pytest.raises(ValueError, match="implementation hash"):
        validate_memory_ab_preflight(path, "longmemeval", "current-code")


def test_memory_ab_preflight_rejects_missing_or_failed_required_suite(tmp_path):
    report = {
        "schema_version": "memory-ab-preflight-v1",
        "dataset": "personalmem",
        "implementation_hash": "implementation-sha",
        "passed": True,
        "suites": [
            {"name": "python_evaluation", "exit_code": 0},
            {"name": "rust_workspace", "exit_code": 0},
            {"name": "diff_check", "exit_code": 0},
        ],
    }
    path = tmp_path / "preflight.json"
    path.write_text(json.dumps(report), encoding="utf-8")

    with pytest.raises(ValueError, match="required suites"):
        validate_memory_ab_preflight(path, "personalmem", "implementation-sha")
