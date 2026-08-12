"""Run one reproducible LoCoMo raw or extracted memory evaluation arm."""

from __future__ import annotations

import argparse
from dataclasses import asdict, dataclass
import hashlib
import json
import os
from pathlib import Path
import shlex
import sys
from typing import Any, Mapping, Sequence

EVALUATION_ROOT = Path(__file__).resolve().parents[1]
PROJECT_ROOT = EVALUATION_ROOT.parent
sys.path.insert(0, str(EVALUATION_ROOT))

from common.memory_ab import (
    ensure_run_mode,
    file_sha256,
    validate_memory_ab_preflight,
)
from common.memory_ab_stage import run_stage
from common.rust_memory_pipeline import (
    MemoryPipelineCommandConfig,
    build_memory_pipeline_command,
)


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
    mode: str = "normal"
    pair_id: str = "standalone"
    promotion_policy_hash: str | None = None
    chat_model: str = "openai/gpt-4o-mini"
    embedding_provider: str = "openrouter"
    embedding_model: str = "baai/bge-m3"
    embedding_dimensions: int = 1024
    embedding_weight: float = 0.7
    bm25_weight: float = 0.3
    candidate_k: int = 150
    rerank_model: str = "cohere/rerank-v3.5"
    rerank_input_k: int = 40
    rerank_enabled: bool = True
    rerank_provider: str = "openrouter"
    rerank_api_key_env: str = "OPENROUTER_API_KEY"
    rerank_base_url: str = OPENROUTER_BASE_URL
    rerank_timeout_ms: int | None = None
    rerank_fail_open: bool = False
    top_k: int = 30
    answer_max_tokens: int = 512
    max_graph_context_facts: int = 3
    max_candidate_tokens: int = 320
    max_window_tokens: int = 640
    context_before_messages: int = 2
    context_after_messages: int = 0
    base_url: str = OPENROUTER_BASE_URL
    credential_env: str = "OPENROUTER_API_KEY"
    graph_enabled: bool = False
    graph_weight: float = 0.2
    graph_rerank: bool = False
    graph_allow_graph_only: bool = False
    graph_max_graph_only_results: int | None = None
    graph_fail_open: bool = False
    graph_memory_space_mode: str = "auto"
    graph_memory_space_field: str = "scope_id"
    graph_owner_id: str = "benchmark"
    graph_llm_api_key_env: str = "OPENROUTER_API_KEY"
    graph_llm_model: str = "openai/gpt-4o-mini"
    graph_llm_base_url: str = OPENROUTER_BASE_URL
    graph_llm_timeout_ms: int = 60000
    graph_build_concurrency: int = 1

    def __post_init__(self) -> None:
        if self.memory_mode not in {"raw", "extracted"}:
            raise ValueError(f"unsupported MEMORY_MODE: {self.memory_mode}")
        if self.phase != "full":
            raise ValueError(f"unsupported PHASE: {self.phase}; only full is supported")
        if self.mode not in {"normal", "strict"}:
            raise ValueError(f"unsupported RUN_MODE: {self.mode}")
        if self.max_graph_context_facts < 0:
            raise ValueError("MAX_GRAPH_CONTEXT_FACTS must be at least 0")
        if self.graph_max_graph_only_results is not None and self.graph_max_graph_only_results < 0:
            raise ValueError("GRAPH_MAX_GRAPH_ONLY_RESULTS must be non-negative")
        if self.graph_build_concurrency < 1:
            raise ValueError("GRAPH_BUILD_CONCURRENCY must be at least 1")
        if self.graph_rerank and not self.graph_enabled:
            raise ValueError("GRAPH_RERANK requires MEMORY_BENCH_GRAPH")
        if self.graph_allow_graph_only and not self.graph_rerank:
            raise ValueError("GRAPH_ALLOW_GRAPH_ONLY requires GRAPH_RERANK")
        if self.graph_max_graph_only_results is not None and not self.graph_allow_graph_only:
            raise ValueError(
                "GRAPH_MAX_GRAPH_ONLY_RESULTS requires GRAPH_ALLOW_GRAPH_ONLY"
            )

    @classmethod
    def from_env(cls, overrides: Mapping[str, str] | None = None) -> "RunConfig":
        values = dict(os.environ)
        if overrides:
            values.update(overrides)
        phase = values.get("PHASE", "full")
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
        policy_path = values.get("PROMOTION_POLICY")
        max_graph_only = values.get("GRAPH_MAX_GRAPH_ONLY_RESULTS", "")
        rerank_timeout = values.get("RERANK_TIMEOUT_MS", "")
        return cls(
            memory_mode=memory_mode,
            phase=phase,
            dataset=dataset,
            run_dir=run_dir,
            mode=values.get("RUN_MODE", "normal"),
            pair_id=values.get("PAIR_ID", "standalone"),
            promotion_policy_hash=(
                file_sha256(Path(policy_path).resolve()) if policy_path else None
            ),
            chat_model=values.get("MODEL", "openai/gpt-4o-mini"),
            embedding_provider=values.get("EMBEDDING_PROVIDER", "openrouter"),
            embedding_model=values.get("EMBEDDING_MODEL", "baai/bge-m3"),
            embedding_dimensions=int(values.get("EMBEDDING_DIMENSIONS", "1024")),
            embedding_weight=float(values.get("EMBEDDING_WEIGHT", "0.7")),
            bm25_weight=float(values.get("BM25_WEIGHT", "0.3")),
            candidate_k=int(values.get("CANDIDATE_K", "150")),
            top_k=int(values.get("TOP_K", "30")),
            rerank_enabled=_truthy(values.get("RERANK", "1")),
            max_graph_context_facts=int(values.get("MAX_GRAPH_CONTEXT_FACTS", "3")),
            graph_enabled=_truthy(values.get("MEMORY_BENCH_GRAPH", "0")),
            graph_weight=float(values.get("GRAPH_WEIGHT", "0.2")),
            graph_rerank=_truthy(values.get("GRAPH_RERANK", "0")),
            graph_allow_graph_only=_truthy(values.get("GRAPH_ALLOW_GRAPH_ONLY", "0")),
            graph_max_graph_only_results=(
                int(max_graph_only) if max_graph_only.strip() else None
            ),
            graph_fail_open=_truthy(values.get("GRAPH_FAIL_OPEN", "0")),
            graph_memory_space_mode=values.get("GRAPH_MEMORY_SPACE_MODE", "auto"),
            graph_memory_space_field=values.get("GRAPH_MEMORY_SPACE_FIELD", "scope_id"),
            graph_owner_id=values.get("GRAPH_OWNER_ID", "benchmark"),
            graph_llm_api_key_env=values.get("GRAPH_LLM_API_KEY_ENV", "OPENROUTER_API_KEY"),
            graph_llm_model=values.get("GRAPH_LLM_MODEL", "openai/gpt-4o-mini"),
            graph_llm_base_url=values.get("GRAPH_LLM_BASE_URL", OPENROUTER_BASE_URL),
            graph_llm_timeout_ms=int(values.get("GRAPH_LLM_TIMEOUT_MS", "60000")),
            graph_build_concurrency=int(values.get("GRAPH_BUILD_CONCURRENCY", "1")),
            rerank_model=values.get("RERANK_MODEL", "cohere/rerank-v3.5"),
            rerank_input_k=int(values.get("RERANK_INPUT_K", "40")),
            rerank_provider=values.get("RERANK_PROVIDER", "openrouter"),
            rerank_api_key_env=values.get("RERANK_API_KEY_ENV", "OPENROUTER_API_KEY"),
            rerank_base_url=values.get("RERANK_BASE_URL", OPENROUTER_BASE_URL),
            rerank_timeout_ms=(int(rerank_timeout) if rerank_timeout.strip() else None),
            rerank_fail_open=_truthy(values.get("RERANK_FAIL_OPEN", "0")),
            answer_max_tokens=int(values.get("ANSWER_MAX_TOKENS", "512")),
            base_url=values.get(
                "LLM_BASE_URL",
                values.get("OPENAI_BASE_URL", OPENROUTER_BASE_URL),
            ),
            credential_env=values.get("EMBEDDING_API_KEY_ENV", "OPENROUTER_API_KEY"),
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
        for key in (
            "memory_mode",
            "phase",
            "dataset",
            "run_dir",
            "pair_id",
            "max_graph_context_facts",
        ):
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


def validate_preflight(config: RunConfig, preflight_path: Path) -> str:
    preflight_path = Path(preflight_path)
    report = json.loads(preflight_path.read_text(encoding="utf-8"))
    if report.get("schema_version") == "memory-ab-preflight-v1":
        return validate_memory_ab_preflight(
            preflight_path,
            "locomo",
            config.immutable_manifest()["implementation_hash"],
        )
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
    return file_sha256(preflight_path)


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
        config.embedding_provider,
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
    command_config = MemoryPipelineCommandConfig(
        project_root=PROJECT_ROOT,
        cache_dir=config.run_dir / "cache" / "memory-pipeline",
        cache_version=configuration_digest,
        model=config.chat_model,
        verifier_model=config.chat_model,
        api_key_env=config.credential_env,
        base_url=config.base_url,
        max_candidate_tokens=config.max_candidate_tokens,
        max_window_tokens=config.max_window_tokens,
        context_before_messages=config.context_before_messages,
        context_after_messages=config.context_after_messages,
        episode_boundary_fields=("session_id",),
        fail_fast=False,
    )
    return build_memory_pipeline_command(
        command_config,
        raw_prepared,
        indexed_prepared,
        artifacts,
    )


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
    command = [*memory_bench_base_command(config, store)]
    if config.graph_enabled:
        command.extend(_graph_search_args(config))
    if config.rerank_enabled:
        command.extend([
            "--rerank",
            "--rerank-provider",
            config.rerank_provider,
            "--rerank-model",
            config.rerank_model,
            "--rerank-api-key-env",
            config.rerank_api_key_env,
            "--rerank-base-url",
            config.rerank_base_url,
            "--rerank-input-k",
            str(config.rerank_input_k),
        ])
        if config.rerank_timeout_ms is not None:
            command.extend(["--rerank-timeout-ms", str(config.rerank_timeout_ms)])
        if config.rerank_fail_open:
            command.append("--rerank-fail-open")
    command.extend([
        "search",
        "--dataset",
        str(indexed_prepared),
        "--top-k",
        str(config.top_k),
        "--output",
        str(search_results),
        "--resume",
    ])
    return command


def build_add_command(
    config: RunConfig,
    store: Path,
    indexed_prepared: Path,
) -> list[str]:
    command = [*memory_bench_base_command(config, store)]
    if config.graph_enabled:
        command.extend([
            "--graph-build",
            "--graph-build-concurrency",
            str(config.graph_build_concurrency),
        ])
        command.extend(_graph_common_args(config))
    command.extend(["add", "--dataset", str(indexed_prepared)])
    return command


def _graph_common_args(config: RunConfig) -> list[str]:
    return [
        "--graph-weight", str(config.graph_weight),
        "--graph-memory-space-mode", config.graph_memory_space_mode,
        "--graph-memory-space-field", config.graph_memory_space_field,
        "--graph-owner-id", config.graph_owner_id,
        "--graph-llm-api-key-env", config.graph_llm_api_key_env,
        "--graph-llm-model", config.graph_llm_model,
        "--graph-llm-base-url", config.graph_llm_base_url,
        "--graph-llm-timeout-ms", str(config.graph_llm_timeout_ms),
    ]


def _graph_search_args(config: RunConfig) -> list[str]:
    args = ["--graph", *_graph_common_args(config)]
    if config.graph_rerank:
        args.append("--graph-rerank")
    if config.graph_allow_graph_only:
        args.append("--graph-allow-graph-only")
    if config.graph_max_graph_only_results is not None:
        args.extend([
            "--graph-max-graph-only-results",
            str(config.graph_max_graph_only_results),
        ])
    if config.graph_fail_open:
        args.append("--graph-fail-open")
    return args


def run_arm(config: RunConfig) -> None:
    preflight_path: Path | None = None
    preflight_hash: str | None = None
    if config.mode == "strict":
        preflight_path_value = os.getenv("PREFLIGHT_PATH")
        if not preflight_path_value:
            raise RuntimeError("PREFLIGHT_PATH is required for strict LoCoMo runs")
        preflight_path = Path(preflight_path_value).resolve()
        preflight_hash = validate_preflight(config, preflight_path)

    if not config.dataset.is_file():
        raise ValueError(f"LoCoMo dataset does not exist: {config.dataset}")
    api_key = os.getenv(config.credential_env)
    if not api_key:
        raise RuntimeError(f"missing API key env {config.credential_env}")

    ensure_run_mode(config.run_dir, config.memory_mode)
    source_digest = file_sha256(config.dataset)
    configuration_digest = config_hash(config)
    public_config = config.public_manifest()
    public_config.update(
        {
            "source_hash": source_digest,
            "configuration_hash": configuration_digest,
            "preflight_path": str(preflight_path) if preflight_path else None,
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
    add_command = build_add_command(config, store, indexed_prepared)
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
            "--max-graph-context-facts",
            str(config.max_graph_context_facts),
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
            "--quiet",
        ],
        (qa_metrics, qa_report),
        manifest,
        inputs=(judged,),
    )
    print(f"[done] LoCoMo {config.memory_mode} arm | metrics={qa_metrics} report={qa_report}")


def implementation_hash() -> str:
    roots = (
        EVALUATION_ROOT / "common",
        EVALUATION_ROOT / "locomo",
        PROJECT_ROOT / "crates" / "memory-bench" / "src",
        PROJECT_ROOT / "crates" / "memory-core" / "src",
        PROJECT_ROOT / "crates" / "memory-pipeline" / "src",
    )
    paths = []
    for root in roots:
        suffix = "*.rs" if root.name == "src" else "*.py"
        paths.extend(
            path
            for path in root.rglob(suffix)
            if not path.name.endswith("_test.py")
        )
    for manifest in (
        PROJECT_ROOT / "Cargo.toml",
        PROJECT_ROOT / "Cargo.lock",
        PROJECT_ROOT / "crates" / "memory-pipeline" / "Cargo.toml",
    ):
        if manifest.is_file():
            paths.append(manifest)
    orchestrator = EVALUATION_ROOT / "scripts" / "run_memory_ab.py"
    if orchestrator.is_file():
        paths.append(orchestrator)
    binary_override = os.getenv("MEMORY_PIPELINE_BIN")
    if binary_override:
        binary_path = Path(shlex.split(binary_override)[0]).resolve()
        if binary_path.is_file():
            paths.append(binary_path)
    digest = hashlib.sha256()
    for path in sorted(paths):
        try:
            identity = str(path.relative_to(PROJECT_ROOT))
        except ValueError:
            identity = f"MEMORY_PIPELINE_BIN:{path}"
        digest.update(identity.encode("utf-8"))
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
    parser.add_argument("--phase", choices=("full",), required=True)
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
