"""Run one reproducible LoCoMo raw or extracted memory evaluation arm."""

from __future__ import annotations

import argparse
from dataclasses import asdict, dataclass
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
from typing import Any, Callable, Mapping, Sequence


EVALUATION_ROOT = Path(__file__).resolve().parents[1]
PROJECT_ROOT = EVALUATION_ROOT.parent
OPENROUTER_BASE_URL = "https://openrouter.ai/api/v1"
PROMPT_VERSIONS = {
    "extraction": "extract_v2",
    "grounding": "ground_v1",
    "answer": "locomo_answer_v1",
    "judge": "locomo_accuracy_v1",
}
EXTRACTION_SCHEMA_VERSION = "atomic_memory_v1"
LLM_TEMPERATURE = 0.0
REQUIRED_PREFLIGHT_SUITES = (
    "python_evaluation",
    "rust_workspace",
    "shell_syntax",
    "diff_check",
)


@dataclass(frozen=True)
class RunConfig:
    memory_mode: str
    phase: str
    dataset: Path
    run_dir: Path
    chat_model: str = "openai/gpt-4o-mini"
    embedding_model: str = "baai/bge-m3"
    embedding_dimensions: int = 1024
    embedding_weight: float = 0.7
    bm25_weight: float = 0.3
    candidate_k: int = 150
    rerank_model: str = "cohere/rerank-v3.5"
    rerank_input_k: int = 40
    top_k: int = 30
    answer_max_tokens: int = 512
    max_candidate_tokens: int = 320
    max_window_tokens: int = 640
    context_before_messages: int = 2
    context_after_messages: int = 0
    base_url: str = OPENROUTER_BASE_URL
    credential_env: str = "OPENROUTER_API_KEY"

    def __post_init__(self) -> None:
        if self.memory_mode not in {"raw", "extracted"}:
            raise ValueError(f"unsupported MEMORY_MODE: {self.memory_mode}")
        if self.phase not in {"pilot", "full"}:
            raise ValueError(f"unsupported PHASE: {self.phase}")

    @classmethod
    def from_env(cls, overrides: Mapping[str, str] | None = None) -> "RunConfig":
        values = dict(os.environ)
        if overrides:
            values.update(overrides)
        phase = values.get("PHASE", "pilot")
        memory_mode = values.get("MEMORY_MODE", "raw")
        dataset = Path(
            values.get("DATASET", str(PROJECT_ROOT / "data" / "locomo" / "locomo10.json"))
        ).resolve()
        run_dir = Path(
            values.get(
                "RUN_DIR",
                str(EVALUATION_ROOT / "outputs" / "locomo-memory-ab" / phase / memory_mode),
            )
        ).resolve()
        return cls(
            memory_mode=memory_mode,
            phase=phase,
            dataset=dataset,
            run_dir=run_dir,
        )

    def public_manifest(self) -> dict[str, Any]:
        value = asdict(self)
        value["dataset"] = str(self.dataset)
        value["run_dir"] = str(self.run_dir)
        value["prompt_versions"] = dict(PROMPT_VERSIONS)
        value["extraction_schema_version"] = EXTRACTION_SCHEMA_VERSION
        value["llm_temperature"] = LLM_TEMPERATURE
        value["implementation_hash"] = implementation_hash()
        return value

    def immutable_manifest(self) -> dict[str, Any]:
        value = self.public_manifest()
        for key in ("memory_mode", "phase", "dataset", "run_dir"):
            value.pop(key, None)
        return value


def config_hash(config: RunConfig) -> str:
    return _json_hash(config.immutable_manifest())


def stage_manifest(name: str, source_hash: str, config_hash: str) -> dict[str, str]:
    return {
        "stage": name,
        "source_hash": source_hash,
        "configuration_hash": config_hash,
    }


def run_stage(
    name: str,
    command: list[str],
    outputs: tuple[Path, ...],
    manifest: dict[str, Any],
    env_overrides: dict[str, str] | None = None,
    inputs: tuple[Path, ...] = (),
    clean_outputs_on_rerun: bool = False,
    runner: Callable[..., subprocess.CompletedProcess] = subprocess.run,
) -> None:
    if not outputs:
        raise ValueError(f"stage {name} must declare at least one output")
    outputs = tuple(Path(path) for path in outputs)
    complete_path = outputs[0].parent / "stages" / f"{name}.complete.json"
    expected = dict(manifest)
    expected["stage"] = name
    expected["command_hash"] = _json_hash(command)
    missing_inputs = [str(path) for path in inputs if not Path(path).is_file()]
    if missing_inputs:
        raise ValueError(f"stage {name} is missing inputs: {missing_inputs}")
    expected["inputs"] = {
        str(path): _file_hash(Path(path))
        for path in inputs
    }
    if _stage_is_complete(complete_path, expected, outputs):
        print(f"[stage {name}] resume hit")
        return

    complete_path.unlink(missing_ok=True)
    if clean_outputs_on_rerun:
        for output in outputs:
            output.unlink(missing_ok=True)
            Path(str(output) + "-shm").unlink(missing_ok=True)
            Path(str(output) + "-wal").unlink(missing_ok=True)
    for output in outputs:
        output.parent.mkdir(parents=True, exist_ok=True)
    child_env = dict(os.environ)
    if env_overrides:
        child_env.update(env_overrides)
    print(f"[stage {name}] running: {shlex.join(command)}")
    runner(
        command,
        cwd=EVALUATION_ROOT,
        env=child_env,
        check=True,
    )
    missing = [str(path) for path in outputs if not path.is_file()]
    if missing:
        raise RuntimeError(f"stage {name} did not produce outputs: {missing}")
    completed = dict(expected)
    completed["outputs"] = {
        str(path): _file_hash(path)
        for path in outputs
    }
    _write_json_atomic(complete_path, completed)


def validate_frozen_config(config: RunConfig, frozen_path: Path) -> None:
    frozen = json.loads(Path(frozen_path).read_text(encoding="utf-8"))
    expected = config.immutable_manifest()
    actual = {
        key: frozen.get(key)
        for key in expected
    }
    if actual != expected:
        differing = sorted(key for key in expected if actual.get(key) != expected[key])
        raise ValueError(
            "frozen configuration mismatch for fields: " + ", ".join(differing)
        )


def validate_preflight(config: RunConfig, preflight_path: Path) -> str:
    preflight_path = Path(preflight_path)
    report = json.loads(preflight_path.read_text(encoding="utf-8"))
    if report.get("schema_version") != "locomo-preflight-v1":
        raise ValueError("preflight has unsupported schema version")
    if not report.get("passed"):
        raise ValueError("preflight did not pass")
    expected_hash = config.immutable_manifest()["implementation_hash"]
    if report.get("implementation_hash") != expected_hash:
        raise ValueError("preflight implementation hash does not match current code")
    suites = report.get("suites") or []
    by_name = {item.get("name"): item for item in suites}
    if set(by_name) != set(REQUIRED_PREFLIGHT_SUITES) or any(
        int(by_name[name].get("exit_code", 1)) != 0
        for name in REQUIRED_PREFLIGHT_SUITES
    ):
        raise ValueError("preflight required suites are incomplete or failed")
    return _file_hash(preflight_path)


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


def memory_bench_base_command(config: RunConfig, store: Path) -> list[str]:
    return [
        "cargo",
        "run",
        "--quiet",
        "--manifest-path",
        str(PROJECT_ROOT / "Cargo.toml"),
        "-p",
        "memory-bench",
        "--",
        "--store",
        str(store),
        "--store-backend",
        "sqlite",
        "--embedding",
        "openrouter",
        "--api-key-env",
        config.credential_env,
        "--model",
        config.embedding_model,
        "--dimensions",
        str(config.embedding_dimensions),
        "--search-mode",
        "hybrid",
        "--embedding-weight",
        str(config.embedding_weight),
        "--bm25-weight",
        str(config.bm25_weight),
        "--candidate-k",
        str(config.candidate_k),
    ]


def build_extraction_command(
    config: RunConfig,
    raw_prepared: Path,
    indexed_prepared: Path,
    artifacts: Path,
    configuration_digest: str,
) -> list[str]:
    """Build the memory-pipeline extraction command for the extracted arm.

    ``--no-fail-fast`` keeps a single transient model error (for example a
    grounding verdict the verifier omits) from aborting the whole arm: the
    pipeline quarantines that window's candidates and continues.
    """
    return [
        sys.executable,
        "-m",
        "common.memory_pipeline.cli",
        "--input",
        str(raw_prepared),
        "--output",
        str(indexed_prepared),
        "--artifacts-dir",
        str(artifacts),
        "--model",
        config.chat_model,
        "--verifier-model",
        config.chat_model,
        "--api-key-env",
        config.credential_env,
        "--base-url",
        config.base_url,
        "--cache-dir",
        str(config.run_dir / "cache" / "memory-pipeline"),
        "--cache-version",
        configuration_digest,
        "--episode-boundary-field",
        "session_id",
        "--max-candidate-tokens",
        str(config.max_candidate_tokens),
        "--max-window-tokens",
        str(config.max_window_tokens),
        "--context-before-messages",
        str(config.context_before_messages),
        "--context-after-messages",
        str(config.context_after_messages),
        "--no-fail-fast",
    ]


def build_search_command(
    config: RunConfig,
    store: Path,
    indexed_prepared: Path,
    search_results: Path,
) -> list[str]:
    """Build the memory-bench search command for either arm.

    ``--resume`` lets a search interrupted mid-batch recover already-completed
    queries from the output file and only re-search the rest, instead of
    restarting all queries from scratch.
    """
    return [
        *memory_bench_base_command(config, store),
        "--rerank",
        "--rerank-provider",
        "openrouter",
        "--rerank-model",
        config.rerank_model,
        "--rerank-api-key-env",
        config.credential_env,
        "--rerank-base-url",
        config.base_url,
        "--rerank-input-k",
        str(config.rerank_input_k),
        "search",
        "--dataset",
        str(indexed_prepared),
        "--top-k",
        str(config.top_k),
        "--output",
        str(search_results),
        "--resume",
    ]


def run_arm(config: RunConfig) -> None:
    if not config.dataset.is_file():
        raise ValueError(f"LoCoMo dataset does not exist: {config.dataset}")
    api_key = os.getenv(config.credential_env)
    if not api_key:
        raise RuntimeError(f"missing API key env {config.credential_env}")
    if config.phase == "full":
        frozen_path = os.getenv("FROZEN_CONFIG")
        if not frozen_path:
            raise RuntimeError("FROZEN_CONFIG is required for a full run")
        validate_frozen_config(config, Path(frozen_path))

    preflight_path_value = os.getenv("PREFLIGHT_PATH")
    if not preflight_path_value:
        raise RuntimeError("PREFLIGHT_PATH is required for LoCoMo runs")
    preflight_path = Path(preflight_path_value).resolve()
    preflight_hash = validate_preflight(config, preflight_path)

    ensure_run_mode(config.run_dir, config.memory_mode)
    source_digest = _file_hash(config.dataset)
    configuration_digest = config_hash(config)
    public_config = config.public_manifest()
    public_config.update(
        {
            "source_hash": source_digest,
            "configuration_hash": configuration_digest,
            "preflight_path": str(preflight_path),
            "preflight_hash": preflight_hash,
            "regression_passed": True,
        }
    )
    _write_json_atomic(config.run_dir / "config.json", public_config)
    manifest = stage_manifest(config.memory_mode, source_digest, configuration_digest)

    raw_prepared = config.run_dir / "raw_prepared.json"
    adapter_command = [
        sys.executable,
        "locomo/locomo_adapter.py",
        "--dataset",
        str(config.dataset),
        "--output",
        str(raw_prepared),
    ]
    if config.phase == "pilot":
        adapter_command.extend(["--sample-index", "0"])
    run_stage(
        "adapter",
        adapter_command,
        (raw_prepared,),
        manifest,
        inputs=(config.dataset,),
    )

    indexed_prepared = raw_prepared
    if config.memory_mode == "extracted":
        indexed_prepared = config.run_dir / "extracted_prepared.json"
        artifacts = config.run_dir / "artifacts"
        extraction_command = build_extraction_command(
            config,
            raw_prepared,
            indexed_prepared,
            artifacts,
            configuration_digest,
        )
        run_stage(
            "extract",
            extraction_command,
            (
                indexed_prepared,
                artifacts / "extraction_stats.json",
                artifacts / "run_metadata.json",
                artifacts / "prepared.json",
            ),
            manifest,
            inputs=(raw_prepared,),
        )

    store = config.run_dir / (
        f"store-{config.memory_mode}-{source_digest[:10]}-"
        f"{configuration_digest[:10]}.sqlite"
    )
    add_command = [
        *memory_bench_base_command(config, store),
        "add",
        "--dataset",
        str(indexed_prepared),
    ]
    run_stage(
        "add",
        add_command,
        (store,),
        manifest,
        inputs=(indexed_prepared,),
        clean_outputs_on_rerun=True,
    )

    search_results = config.run_dir / "search_results.json"
    search_command = build_search_command(
        config, store, indexed_prepared, search_results
    )
    run_stage(
        "search",
        search_command,
        (search_results,),
        manifest,
        inputs=(indexed_prepared, store),
    )

    retrieval_metrics = config.run_dir / "retrieval_metrics.json"
    retrieval_report = config.run_dir / "retrieval_report.html"
    run_stage(
        "retrieval",
        [
            sys.executable,
            "locomo/locomo_retrieval.py",
            "--dataset",
            str(config.dataset),
            "--input",
            str(search_results),
            "--input-format",
            f"prepared-{config.memory_mode}",
            "--output-json",
            str(retrieval_metrics),
            "--html-report",
            str(retrieval_report),
        ],
        (retrieval_metrics, retrieval_report),
        manifest,
        inputs=(config.dataset, search_results),
    )

    responses = config.run_dir / "responses.json"
    answer_stats = config.run_dir / "responses_answer_stats.json"
    answer_env = {
        "OPENAI_API_KEY": api_key,
        "OPENAI_BASE_URL": config.base_url,
        "MODEL": config.chat_model,
        "ANSWER_MAX_TOKENS": str(config.answer_max_tokens),
    }
    run_stage(
        "answer",
        [
            sys.executable,
            "locomo/locomo_responses.py",
            "--technique-type",
            "prepared_memory",
            "--dataset",
            str(config.dataset),
            "--prepared-source",
            str(raw_prepared),
            "--memory-mode",
            config.memory_mode,
            "--input",
            str(search_results),
            "--output",
            str(responses),
            "--cache-dir",
            str(config.run_dir / "cache" / "answer"),
            "--cache-version",
            configuration_digest,
        ],
        (responses, answer_stats),
        manifest,
        env_overrides=answer_env,
        inputs=(config.dataset, raw_prepared, search_results),
    )

    judged = config.run_dir / "judge_results.json"
    run_stage(
        "judge",
        [
            sys.executable,
            "locomo/locomo_eval.py",
            "--input",
            str(responses),
            "--output",
            str(judged),
            "--judge-model",
            config.chat_model,
            "--llm-api-key-env",
            config.credential_env,
            "--llm-base-url",
            config.base_url,
            "--cache-dir",
            str(config.run_dir / "cache" / "judge"),
            "--cache-version",
            configuration_digest,
        ],
        (judged,),
        manifest,
        inputs=(responses,),
    )

    qa_metrics = config.run_dir / "qa_metrics.json"
    qa_report = config.run_dir / "qa_report.html"
    run_stage(
        "metrics",
        [
            sys.executable,
            "locomo/locomo_metric.py",
            "--input",
            str(judged),
            "--output-json",
            str(qa_metrics),
            "--html-report",
            str(qa_report),
        ],
        (qa_metrics, qa_report),
        manifest,
        inputs=(judged,),
    )


def _stage_is_complete(
    path: Path,
    expected: dict[str, Any],
    outputs: tuple[Path, ...],
) -> bool:
    if not path.is_file() or not all(output.is_file() for output in outputs):
        return False
    try:
        completed = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return False
    for key, value in expected.items():
        if completed.get(key) != value:
            return False
    hashes = completed.get("outputs") or {}
    return all(hashes.get(str(output)) == _file_hash(output) for output in outputs)


def _file_hash(path: Path) -> str:
    digest = hashlib.sha256()
    with Path(path).open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def implementation_hash() -> str:
    roots = (
        EVALUATION_ROOT / "common",
        EVALUATION_ROOT / "locomo",
        PROJECT_ROOT / "crates" / "memory-bench" / "src",
        PROJECT_ROOT / "crates" / "memory-core" / "src",
    )
    paths = []
    for root in roots:
        suffix = "*.rs" if root.name == "src" else "*.py"
        paths.extend(
            path
            for path in root.rglob(suffix)
            if not path.name.endswith("_test.py")
        )
    cargo_lock = PROJECT_ROOT / "Cargo.lock"
    if cargo_lock.is_file():
        paths.append(cargo_lock)
    digest = hashlib.sha256()
    for path in sorted(paths):
        digest.update(str(path.relative_to(PROJECT_ROOT)).encode("utf-8"))
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def _json_hash(value: Any) -> str:
    payload = json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


def _write_json_atomic(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(
        json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n",
        encoding="utf-8",
    )
    temporary.replace(path)


def _truthy(value: str) -> bool:
    return value.lower() in {"1", "true", "yes", "on"}


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Run one LoCoMo memory A/B arm.")
    parser.add_argument("--phase", choices=("pilot", "full"), required=True)
    parser.add_argument("--run-dir", type=Path, required=True)
    parser.add_argument("--dataset", type=Path)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    overrides = {
        "PHASE": args.phase,
        "RUN_DIR": str(args.run_dir),
    }
    if args.dataset is not None:
        overrides["DATASET"] = str(args.dataset)
    run_arm(RunConfig.from_env(overrides))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
