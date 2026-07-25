"""Shared contracts for paired raw and extracted memory experiments."""

from __future__ import annotations

import hashlib
import json
from dataclasses import dataclass
from pathlib import Path
from typing import Any


MEMORY_AB_PREFLIGHT_SCHEMA_VERSION = "memory-ab-preflight-v1"
MEMORY_AB_PREFLIGHT_SUITES = frozenset(
    {
        "python_evaluation",
        "rust_workspace",
        "rust_clippy",
        "diff_check",
    }
)


@dataclass(frozen=True)
class ExperimentArmConfig:
    dataset: str
    phase: str
    memory_mode: str
    source_path: Path
    run_dir: Path
    immutable: dict[str, Any]

    def public_manifest(self) -> dict[str, Any]:
        return {
            "dataset": self.dataset,
            "phase": self.phase,
            "memory_mode": self.memory_mode,
            "source_path": str(self.source_path),
            "run_dir": str(self.run_dir),
            **self.immutable,
            "configuration_hash": canonical_sha256(self.immutable),
        }


def canonical_sha256(value: Any) -> str:
    payload = json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with Path(path).open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def validate_pair_contract(
    raw: dict[str, Any],
    extracted: dict[str, Any],
    raw_prepared: dict[str, Any],
    extracted_prepared: dict[str, Any],
) -> dict[str, Any]:
    if raw["memory_mode"] != "raw" or extracted["memory_mode"] != "extracted":
        raise ValueError("paired arms must declare raw and extracted memory modes")
    for key in (
        "source_hash",
        "configuration_hash",
        "implementation_hash",
        "preflight_hash",
    ):
        if not raw.get(key) or raw[key] != extracted.get(key):
            raise ValueError(f"raw/extracted {key} mismatch")
    queries = raw_prepared.get("queries")
    if not isinstance(queries, list) or queries != extracted_prepared.get("queries"):
        raise ValueError("raw/extracted prepared queries differ")
    ids = [str(query.get("id") or "") for query in queries]
    if not all(ids) or len(ids) != len(set(ids)):
        raise ValueError("prepared query ids are missing or duplicated")
    return {
        "query_count": len(ids),
        "configuration_hash": raw["configuration_hash"],
    }


def validate_frozen_manifest(current_immutable: dict[str, Any], frozen_path: Path) -> None:
    frozen = json.loads(Path(frozen_path).read_text(encoding="utf-8"))
    actual = {key: frozen.get(key) for key in current_immutable}
    if actual != current_immutable:
        differing = sorted(
            key
            for key in current_immutable
            if actual.get(key) != current_immutable[key]
        )
        raise ValueError(
            "frozen configuration mismatch for fields: " + ", ".join(differing)
        )


def validate_memory_ab_preflight(
    path: Path,
    dataset: str,
    implementation_hash: str,
) -> str:
    """Validate a complete dataset-bound regression gate and return its SHA-256."""
    path = Path(path)
    try:
        report = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"could not read memory A/B preflight: {path}") from error
    if not isinstance(report, dict):
        raise ValueError("memory A/B preflight must be a JSON object")
    if report.get("schema_version") != MEMORY_AB_PREFLIGHT_SCHEMA_VERSION:
        raise ValueError("memory A/B preflight has unsupported schema version")
    if report.get("dataset") != dataset:
        raise ValueError("memory A/B preflight dataset mismatch")
    if report.get("implementation_hash") != implementation_hash:
        raise ValueError("memory A/B preflight implementation hash mismatch")
    if report.get("passed") is not True:
        raise ValueError("memory A/B preflight did not pass")
    suites = report.get("suites")
    if not isinstance(suites, list) or any(
        not isinstance(item, dict) for item in suites
    ):
        raise ValueError("memory A/B preflight required suites are invalid")
    names = [item.get("name") for item in suites]
    if (
        len(names) != len(set(names))
        or set(names) != MEMORY_AB_PREFLIGHT_SUITES
        or any(item.get("exit_code") != 0 for item in suites)
    ):
        raise ValueError("memory A/B preflight required suites are incomplete or failed")
    return file_sha256(path)


def ensure_run_mode(run_dir: Path, memory_mode: str) -> None:
    run_dir = Path(run_dir)
    run_dir.mkdir(parents=True, exist_ok=True)
    sentinel = run_dir / ".memory_mode"
    if sentinel.is_file():
        existing = sentinel.read_text(encoding="utf-8").strip()
        if existing != memory_mode:
            raise ValueError(
                f"run directory already belongs to memory mode {existing}; "
                f"cannot reuse it for {memory_mode}"
            )
        return
    temporary = sentinel.with_suffix(".tmp")
    temporary.write_text(memory_mode + "\n", encoding="utf-8")
    temporary.replace(sentinel)


def ensure_store_mode(store: Path, memory_mode: str) -> None:
    """Claim a store for one memory mode and reject cross-arm reuse."""
    store = Path(store).resolve()
    sentinel = Path(f"{store}.memory_mode")
    sentinel.parent.mkdir(parents=True, exist_ok=True)
    if memory_mode == "extracted" and store.exists() and not sentinel.exists():
        raise ValueError(
            "existing unowned store cannot be claimed by extracted; "
            "use a new store or explicitly select raw for legacy raw migration"
        )
    try:
        with sentinel.open("x", encoding="utf-8") as target:
            target.write(memory_mode + "\n")
        return
    except FileExistsError:
        existing = sentinel.read_text(encoding="utf-8").strip()
        if existing != memory_mode:
            raise ValueError(
                f"store already belongs to memory mode {existing}; "
                f"cannot reuse it for {memory_mode}"
            )
