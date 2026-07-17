import argparse
import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import personalmem.run as personalmem_run
from personalmem.run import (
    build_parser,
    build_prepared_schema_v1,
    run_prepare,
    write_personamem_run_meta,
)


def _legacy_fixture():
    return {
        "source": "bowen-upenn/PersonaMem",
        "conversation": [
            {
                "id": "ctx-1:0",
                "shared_context_id": "ctx-1",
                "speaker": "user",
                "text": "I prefer morning flights.",
            }
        ],
        "questions": [
            {
                "question_id": "q-1",
                "shared_context_id": "ctx-1",
                "question_type": "preference",
                "topic": "travel",
                "question": "When should the flight leave?",
                "answer": "In the morning.",
                "correct_answer": "(a)",
                "all_options": "['(a) In the morning', '(b) At night']",
            }
        ],
    }


def _args(**overrides):
    values = {
        "memory_mode": "raw",
        "dataset": Path("raw.json"),
        "indexed_dataset": Path("extracted.json"),
    }
    values.update(overrides)
    return argparse.Namespace(**values)


def _parse(*arguments):
    return build_parser().parse_args(arguments)


def _write_preflight(path: Path, dataset: str, implementation_hash: str) -> None:
    path.write_text(
        json.dumps(
            {
                "schema_version": "memory-ab-preflight-v1",
                "dataset": dataset,
                "implementation_hash": implementation_hash,
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
        ),
        encoding="utf-8",
    )


FIXTURES = Path(__file__).parents[1] / "fixtures"
PERSONALMEM_FIXTURE = FIXTURES / "personalmem_sample.json"
EXTRACTOR_FIXTURE = FIXTURES / "personalmem_memory_extractor_responses.json"
GROUNDING_FIXTURE = FIXTURES / "personalmem_memory_grounding_responses.json"


def test_memory_ab_pipeline_cli_accepts_a_validated_preflight(tmp_path):
    args = _parse(
        "memory-ab-pipeline",
        "--dataset",
        str(tmp_path / "prepared.json"),
        "--preflight",
        str(tmp_path / "preflight.json"),
    )

    assert args.command == "memory-ab-pipeline"
    assert args.preflight == tmp_path / "preflight.json"


def test_personalmem_arm_contract_contains_real_preflight_hash_before_stages(
    monkeypatch,
    tmp_path,
):
    prepared = _prepared_sample_fixture()
    source = tmp_path / "prepared.json"
    source.write_text(json.dumps(prepared), encoding="utf-8")
    policy = tmp_path / "policy.json"
    policy.write_text("{}\n", encoding="utf-8")
    preflight = tmp_path / "preflight.json"
    implementation = "a" * 64
    _write_preflight(preflight, "personalmem", implementation)
    run_dir = tmp_path / "raw"
    args = _parse(
        "memory-ab-pipeline",
        "--dataset",
        str(source),
        "--run-dir",
        str(run_dir),
        "--promotion-policy",
        str(policy),
        "--preflight",
        str(preflight),
        "--embedding",
        "hash",
    )
    calls = []
    monkeypatch.setattr(personalmem_run, "build_parser", lambda: argparse.Namespace(parse_args=lambda: args))
    monkeypatch.setattr(personalmem_run, "implementation_hash", lambda: implementation)
    monkeypatch.setattr(
        personalmem_run,
        "resolve_indexed_dataset",
        lambda value: calls.append("resolve") or value.dataset,
    )
    for name in ("add", "search", "eval", "answer", "grade"):
        monkeypatch.setattr(
            personalmem_run,
            f"run_{name}",
            lambda value, stage=name: calls.append(stage) or 0,
        )

    assert personalmem_run.main() == 0

    config = json.loads((run_dir / "config.json").read_text(encoding="utf-8"))
    assert config["preflight_hash"] == personalmem_run.file_sha256(preflight)
    assert json.loads((run_dir / "raw_prepared.json").read_text(encoding="utf-8")) == prepared
    assert calls == ["resolve", "add", "search", "eval", "answer", "grade"]


def _prepared_sample_fixture():
    sample = json.loads(PERSONALMEM_FIXTURE.read_text(encoding="utf-8"))
    scope_id = "personalmem-sample"
    legacy = {
        "source": "bowen-upenn/PersonaMem",
        "conversation": [
            {
                "id": f"{scope_id}:{index}",
                "shared_context_id": scope_id,
                "speaker": message["speaker"],
                "text": message["text"],
            }
            for index, message in enumerate(sample["conversation"])
        ],
        "questions": [
            {
                "question_id": f"personalmem-q-{index}",
                "shared_context_id": scope_id,
                "question_type": "persona",
                "topic": "preference",
                "question": question["question"],
                "answer": question["answer"],
                "correct_answer": "(a)",
                "all_options": [
                    f"(a) {question['answer']}",
                    "(b) None of the above.",
                ],
            }
            for index, question in enumerate(sample["questions"])
        ],
    }
    return build_prepared_schema_v1(legacy, "fixture")


def test_prepared_v1_preserves_scope_role_and_multiple_choice_task():
    prepared = build_prepared_schema_v1(_legacy_fixture(), "32k")

    assert prepared["memories"][0]["metadata"]["scope_id"] == "ctx-1"
    assert prepared["memories"][0]["metadata"]["role"] == "user"
    assert prepared["queries"][0]["task"]["type"] == "multiple_choice"


def test_prepare_always_writes_benchmark_prepared_v1(tmp_path):
    raw_dir = tmp_path / "raw"
    raw_dir.mkdir()
    (raw_dir / "questions_32k.csv").write_text(
        "question_id,shared_context_id,user_question_or_message,question_type,topic,correct_answer,all_options\n"
        'q-1,ctx-1,When?,preference,travel,(a),"[\'(a) Morning\', \'(b) Night\']"\n',
        encoding="utf-8",
    )
    (raw_dir / "shared_contexts_32k.jsonl").write_text(
        json.dumps(
            {
                "shared_context_id": "ctx-1",
                "messages": [{"role": "user", "content": "Morning flights."}],
            }
        )
        + "\n",
        encoding="utf-8",
    )
    output = tmp_path / "prepared.json"
    args = argparse.Namespace(
        raw_dir=raw_dir,
        size="32k",
        limit_questions=0,
        max_context_messages=0,
        prepared_dataset=output,
        schema_version="legacy",
    )

    assert run_prepare(args) == 0
    prepared = json.loads(output.read_text(encoding="utf-8"))
    assert prepared["schema_version"] == "benchmark-prepared-v1"


def test_raw_pipeline_indexes_raw_file():
    args = _args()

    assert personalmem_run.resolve_indexed_dataset(args) == Path("raw.json")


def test_extracted_pipeline_indexes_extracted_file(monkeypatch, tmp_path):
    raw = tmp_path / "raw.json"
    raw.write_text(
        json.dumps(build_prepared_schema_v1(_legacy_fixture(), "32k")),
        encoding="utf-8",
    )
    args = _args(
        memory_mode="extracted",
        dataset=raw,
        indexed_dataset=tmp_path / "extracted.json",
    )
    monkeypatch.setattr(personalmem_run, "run_stage", lambda *args, **kwargs: None)

    assert personalmem_run.resolve_indexed_dataset(args) == tmp_path / "extracted.json"


def test_extracted_pipeline_requires_prepared_v1(monkeypatch, tmp_path):
    raw = tmp_path / "legacy.json"
    raw.write_text(json.dumps(_legacy_fixture()), encoding="utf-8")
    args = _args(
        memory_mode="extracted",
        dataset=raw,
        indexed_dataset=tmp_path / "extracted.json",
    )
    monkeypatch.setattr(personalmem_run, "run_stage", lambda *args, **kwargs: None)

    with pytest.raises(ValueError, match="benchmark-prepared-v1"):
        personalmem_run.resolve_indexed_dataset(args)


@pytest.mark.parametrize("command", ["pipeline", "official-pipeline"])
def test_pipeline_parses_memory_ab_and_extraction_flags(command, tmp_path):
    args = _parse(
        command,
        "--memory-mode",
        "extracted",
        "--phase",
        "full",
        "--pair-id",
        "pair-5",
        "--indexed-dataset",
        str(tmp_path / "indexed.json"),
        "--extractor-responses",
        str(tmp_path / "extractor.json"),
        "--grounding-responses",
        str(tmp_path / "grounding.json"),
        "--max-candidate-tokens",
        "111",
        "--max-window-tokens",
        "222",
        "--context-before-messages",
        "3",
        "--context-after-messages",
        "1",
        "--frozen-config",
        str(tmp_path / "frozen.json"),
        "--promotion-policy",
        str(tmp_path / "policy.json"),
    )

    assert args.memory_mode == "extracted"
    assert args.phase == "full"
    assert args.pair_id == "pair-5"
    assert args.indexed_dataset == tmp_path / "indexed.json"
    assert args.extractor_responses == tmp_path / "extractor.json"
    assert args.grounding_responses == tmp_path / "grounding.json"
    assert args.max_candidate_tokens == 111
    assert args.max_window_tokens == 222
    assert args.context_before_messages == 3
    assert args.context_after_messages == 1
    assert args.frozen_config == tmp_path / "frozen.json"
    assert args.promotion_policy == tmp_path / "policy.json"


def test_fixture_mode_requires_paired_response_maps(tmp_path):
    args = _parse(
        "pipeline",
        "--memory-mode",
        "extracted",
        "--extractor-responses",
        str(tmp_path / "extractor.json"),
    )

    with pytest.raises(ValueError, match="both --extractor-responses and"):
        personalmem_run.validate_experiment_args(args)


def test_full_mode_requires_frozen_config_and_promotion_policy():
    args = _parse("pipeline", "--phase", "full")

    with pytest.raises(ValueError, match="--frozen-config"):
        personalmem_run.validate_experiment_args(args)

    args.frozen_config = Path("frozen.json")
    with pytest.raises(ValueError, match="--promotion-policy"):
        personalmem_run.validate_experiment_args(args)


def test_raw_and_extracted_arms_share_immutable_settings():
    raw = _parse("pipeline", "--memory-mode", "raw")
    extracted = _parse("pipeline", "--memory-mode", "extracted")

    raw_manifest = personalmem_run.immutable_experiment_manifest(
        raw,
        "a" * 64,
        "b" * 64,
    )
    extracted_manifest = personalmem_run.immutable_experiment_manifest(
        extracted,
        "a" * 64,
        "b" * 64,
    )

    assert raw_manifest == extracted_manifest
    assert raw_manifest["promotion_policy_hash"] == "b" * 64
    assert raw_manifest["implementation_hash"] == "a" * 64


def test_answer_retry_settings_are_part_of_immutable_manifest():
    raw = _parse(
        "pipeline",
        "--memory-mode",
        "raw",
        "--max-retries",
        "2",
        "--retry-backoff-seconds",
        "0.5",
    )
    extracted = _parse(
        "pipeline",
        "--memory-mode",
        "extracted",
        "--max-retries",
        "4",
        "--retry-backoff-seconds",
        "1.5",
    )

    raw_manifest = personalmem_run.immutable_experiment_manifest(
        raw,
        "a" * 64,
        "b" * 64,
    )
    extracted_manifest = personalmem_run.immutable_experiment_manifest(
        extracted,
        "a" * 64,
        "b" * 64,
    )

    assert raw_manifest["max_retries"] == 2
    assert extracted_manifest["max_retries"] == 4
    assert raw_manifest["retry_backoff_seconds"] == 0.5
    assert extracted_manifest["retry_backoff_seconds"] == 1.5
    assert raw_manifest != extracted_manifest
    assert personalmem_run.canonical_sha256(raw_manifest) != personalmem_run.canonical_sha256(
        extracted_manifest
    )


def test_implementation_hash_tracks_memory_pipeline_binary(monkeypatch, tmp_path):
    binary = tmp_path / "memory-pipeline"
    binary.write_bytes(b"first")
    monkeypatch.setenv("MEMORY_PIPELINE_BIN", str(binary))

    first = personalmem_run.implementation_hash()
    binary.write_bytes(b"second")

    assert personalmem_run.implementation_hash() != first


def test_full_validation_happens_before_run_dir_or_runner_side_effects(monkeypatch):
    args = _parse("pipeline", "--phase", "full")
    calls = []
    monkeypatch.setattr(personalmem_run, "build_parser", lambda: argparse.Namespace(parse_args=lambda: args))
    monkeypatch.setattr(
        personalmem_run,
        "apply_default_paths",
        lambda value: calls.append("run-dir"),
    )
    monkeypatch.setattr(
        personalmem_run,
        "run_add",
        lambda value: calls.append("add") or 0,
    )

    with pytest.raises(ValueError, match="--frozen-config"):
        personalmem_run.main()

    assert calls == []


def test_full_frozen_mismatch_precedes_dataset_and_pipeline_side_effects(
    monkeypatch,
    tmp_path,
):
    policy = tmp_path / "policy.json"
    policy.write_text('{"minimum_accuracy_delta": 0.0}\n', encoding="utf-8")
    frozen = tmp_path / "frozen.json"
    args = _parse(
        "pipeline",
        "--phase",
        "full",
        "--frozen-config",
        str(frozen),
        "--promotion-policy",
        str(policy),
        "--dataset",
        str(tmp_path / "raw.json"),
    )
    implementation_digest = "a" * 64
    immutable = personalmem_run.immutable_experiment_manifest(
        args,
        implementation_digest,
        personalmem_run.file_sha256(policy),
    )
    immutable["max_retries"] = args.max_retries + 1
    frozen.write_text(json.dumps(immutable), encoding="utf-8")
    calls = []
    monkeypatch.setattr(personalmem_run, "build_parser", lambda: argparse.Namespace(parse_args=lambda: args))
    monkeypatch.setattr(
        personalmem_run,
        "implementation_hash",
        lambda: implementation_digest,
        raising=False,
    )
    monkeypatch.setattr(
        personalmem_run,
        "apply_default_paths",
        lambda value: calls.append("run-dir"),
    )
    monkeypatch.setattr(
        personalmem_run,
        "ensure_store_mode",
        lambda store, memory_mode: calls.append("store"),
    )
    monkeypatch.setattr(
        personalmem_run,
        "resolve_indexed_dataset",
        lambda value: calls.append("extract") or value.dataset,
    )
    monkeypatch.setattr(
        personalmem_run,
        "run_add",
        lambda value: calls.append("add") or 0,
    )
    monkeypatch.setattr(
        personalmem_run,
        "run_search",
        lambda value: calls.append("search") or 0,
    )
    monkeypatch.setattr(
        personalmem_run,
        "run_eval",
        lambda value: calls.append("eval") or 0,
    )

    with pytest.raises(ValueError, match="frozen configuration mismatch.*max_retries"):
        personalmem_run.main()

    assert calls == []


def test_pipeline_reuses_existing_stages_with_resolved_indexed_dataset(
    monkeypatch,
    tmp_path,
):
    raw = tmp_path / "raw.json"
    indexed = tmp_path / "indexed.json"
    args = _parse(
        "pipeline",
        "--dataset",
        str(raw),
        "--indexed-dataset",
        str(indexed),
        "--memory-mode",
        "extracted",
        "--run-dir",
        str(tmp_path / "run"),
    )
    calls = []
    monkeypatch.setattr(personalmem_run, "build_parser", lambda: argparse.Namespace(parse_args=lambda: args))
    monkeypatch.setattr(
        personalmem_run,
        "implementation_hash",
        lambda: "a" * 64,
        raising=False,
    )
    monkeypatch.setattr(
        personalmem_run,
        "resolve_indexed_dataset",
        lambda value: calls.append(("resolve", value.dataset)) or indexed,
    )
    for name in ("add", "search", "eval"):
        monkeypatch.setattr(
            personalmem_run,
            f"run_{name}",
            lambda value, stage=name: calls.append((stage, value.dataset)) or 0,
        )

    assert personalmem_run.main() == 0
    assert calls == [
        ("resolve", raw),
        ("add", indexed),
        ("search", indexed),
        ("eval", indexed),
    ]


def test_default_run_paths_separate_raw_and_extracted_stores(monkeypatch, tmp_path):
    counter = iter(("raw-run", "extracted-run"))
    monkeypatch.setattr(
        personalmem_run,
        "default_run_dir",
        lambda dataset, run_id=None: tmp_path / next(counter) / str(run_id),
    )
    raw = _parse("pipeline", "--memory-mode", "raw")
    extracted = _parse("pipeline", "--memory-mode", "extracted")

    personalmem_run.apply_default_paths(raw)
    personalmem_run.apply_default_paths(extracted)

    assert raw.store != extracted.store
    assert raw.run_dir.name.endswith("_raw")
    assert extracted.run_dir.name.endswith("_extracted")


def test_explicit_store_cannot_be_reused_across_memory_modes(monkeypatch, tmp_path):
    shared_store = tmp_path / "shared.sqlite"
    raw = _parse(
        "pipeline",
        "--dataset",
        str(tmp_path / "raw.json"),
        "--store",
        str(shared_store),
        "--run-dir",
        str(tmp_path / "raw-run"),
        "--memory-mode",
        "raw",
    )
    raw_reuse = _parse(
        "pipeline",
        "--dataset",
        str(tmp_path / "raw.json"),
        "--store",
        str(shared_store),
        "--run-dir",
        str(tmp_path / "raw-reuse-run"),
        "--memory-mode",
        "raw",
    )
    extracted = _parse(
        "pipeline",
        "--dataset",
        str(tmp_path / "raw.json"),
        "--store",
        str(shared_store),
        "--run-dir",
        str(tmp_path / "extracted-run"),
        "--memory-mode",
        "extracted",
    )
    parsed = iter((raw, raw_reuse, extracted))
    calls = []
    monkeypatch.setattr(
        personalmem_run,
        "build_parser",
        lambda: argparse.Namespace(parse_args=lambda: next(parsed)),
    )
    monkeypatch.setattr(
        personalmem_run,
        "implementation_hash",
        lambda: "a" * 64,
    )
    monkeypatch.setattr(
        personalmem_run,
        "resolve_indexed_dataset",
        lambda value: calls.append(("resolve", value.memory_mode)) or value.dataset,
    )
    for name in ("add", "search", "eval"):
        monkeypatch.setattr(
            personalmem_run,
            f"run_{name}",
            lambda value, stage=name: calls.append((stage, value.memory_mode)) or 0,
        )

    assert personalmem_run.main() == 0
    assert personalmem_run.main() == 0
    with pytest.raises(
        ValueError,
        match="store already belongs to memory mode raw; cannot reuse it for extracted",
    ):
        personalmem_run.main()

    assert calls == [
        ("resolve", "raw"),
        ("add", "raw"),
        ("search", "raw"),
        ("eval", "raw"),
        ("resolve", "raw"),
        ("add", "raw"),
        ("search", "raw"),
        ("eval", "raw"),
    ]


def test_offline_extracted_pipeline_preserves_queries_scope_and_memory_kind(
    monkeypatch,
    tmp_path,
):
    raw = _prepared_sample_fixture()
    raw_path = tmp_path / "raw.json"
    raw_path.write_text(json.dumps(raw, ensure_ascii=False), encoding="utf-8")
    run_dir = tmp_path / "run"
    indexed_path = run_dir / "extracted_prepared.json"
    monkeypatch.delenv("OPENROUTER_API_KEY", raising=False)
    args = _parse(
        "pipeline",
        "--dataset",
        str(raw_path),
        "--indexed-dataset",
        str(indexed_path),
        "--run-dir",
        str(run_dir),
        "--memory-mode",
        "extracted",
        "--pair-id",
        "offline-personalmem",
        "--extractor-responses",
        str(EXTRACTOR_FIXTURE),
        "--grounding-responses",
        str(GROUNDING_FIXTURE),
        "--embedding",
        "hash",
        "--model",
        "hash",
        "--dimensions",
        "32",
        "--top-k",
        "2",
    )
    monkeypatch.setattr(personalmem_run, "build_parser", lambda: argparse.Namespace(parse_args=lambda: args))

    assert personalmem_run.main() == 0

    indexed = json.loads(indexed_path.read_text(encoding="utf-8"))
    assert indexed["queries"] == raw["queries"]
    assert indexed["memories"]
    assert all(
        memory["metadata"]["scope_id"] == "personalmem-sample"
        for memory in indexed["memories"]
    )
    assert all(
        memory["metadata"]["memory_kind"] == "extracted_memory"
        for memory in indexed["memories"]
    )


def test_run_meta_uses_ram_a_backend(tmp_path):
    args = argparse.Namespace(
        run_dir=tmp_path,
        report=tmp_path / "retrieval_metrics.json",
        dataset=tmp_path / "prepared.json",
        output=tmp_path / "search_results.json",
        store=tmp_path / "store.sqlite",
        html_report=None,
        responses=tmp_path / "responses.json",
        grades=tmp_path / "grade_metrics.json",
        top_k=10,
        embedding="hash",
        model="hash",
        dimensions=128,
        search_mode="hybrid",
        candidate_k=None,
        embedding_weight=0.7,
        bm25_weight=0.3,
        store_backend="sqlite",
        answer_model="openai/gpt-4o-mini",
        context_token_budget=2000,
        backend="RAM-A",
    )

    meta = write_personamem_run_meta(args, phase="retrieval")

    assert meta["backend"] == "RAM-A"


def test_run_meta_allows_mem0_backend_override(tmp_path):
    args = argparse.Namespace(
        run_dir=tmp_path,
        report=tmp_path / "retrieval_metrics.json",
        dataset=tmp_path / "prepared.json",
        output=tmp_path / "search_results.json",
        store=tmp_path / "mem0_local",
        html_report=None,
        responses=tmp_path / "responses.json",
        grades=tmp_path / "grade_metrics.json",
        top_k=10,
        embedding="openrouter",
        model="baai/bge-m3",
        dimensions=1024,
        search_mode="hybrid",
        candidate_k=None,
        embedding_weight=0.7,
        bm25_weight=0.3,
        store_backend="sqlite",
        answer_model="openai/gpt-4o-mini",
        context_token_budget=2000,
        backend="mem0",
    )

    meta = write_personamem_run_meta(args, phase="grade")

    assert meta["backend"] == "mem0"


def test_run_meta_records_memory_ab_identity_and_hashes(tmp_path):
    args = argparse.Namespace(
        run_dir=tmp_path,
        report=tmp_path / "retrieval_metrics.json",
        dataset=tmp_path / "extracted.json",
        raw_dataset=tmp_path / "raw.json",
        indexed_dataset=tmp_path / "extracted.json",
        output=tmp_path / "search_results.json",
        store=tmp_path / "store.sqlite",
        html_report=None,
        responses=tmp_path / "responses.json",
        grades=tmp_path / "grade_metrics.json",
        top_k=10,
        embedding="hash",
        model="hash",
        dimensions=32,
        search_mode="hybrid",
        candidate_k=None,
        embedding_weight=0.7,
        bm25_weight=0.3,
        store_backend="sqlite",
        answer_model="openai/gpt-4o-mini",
        context_token_budget=2000,
        backend="RAM-A",
        memory_mode="extracted",
        phase="pilot",
        pair_id="pair-5",
        configuration_hash="a" * 64,
        implementation_hash="b" * 64,
        promotion_policy_hash="c" * 64,
        preflight_hash="d" * 64,
    )

    meta = write_personamem_run_meta(args, phase="retrieval")

    assert meta["memory_mode"] == "extracted"
    assert meta["experiment_phase"] == "pilot"
    assert meta["pair_id"] == "pair-5"
    assert meta["source_path"] == str(tmp_path / "raw.json")
    assert meta["indexed_dataset"] == str(tmp_path / "extracted.json")
    assert meta["configuration_hash"] == "a" * 64
    assert meta["implementation_hash"] == "b" * 64
    assert meta["promotion_policy_hash"] == "c" * 64
    assert meta["preflight_hash"] == "d" * 64


def test_raw_run_meta_records_raw_dataset_as_indexed_dataset(tmp_path):
    raw_dataset = tmp_path / "raw.json"
    args = _parse(
        "pipeline",
        "--dataset",
        str(raw_dataset),
        "--run-dir",
        str(tmp_path / "raw-run"),
        "--memory-mode",
        "raw",
        "--embedding",
        "hash",
    )
    personalmem_run.apply_default_paths(args)
    args.dataset = personalmem_run.resolve_indexed_dataset(args)

    meta = write_personamem_run_meta(args, phase="retrieval")

    assert meta["source_path"] == str(raw_dataset)
    assert meta["indexed_dataset"] == str(raw_dataset)


def test_parser_accepts_graph_flags():
    parser = personalmem_run.build_parser()

    args = parser.parse_args([
        "search",
        "--dataset",
        "data.json",
        "--graph",
        "--graph-build",
        "--graph-weight",
        "0.4",
        "--graph-fail-open",
        "--graph-memory-space-mode",
        "metadata-field",
        "--graph-memory-space-field",
        "tenant_id",
        "--graph-owner-id",
        "bench-owner",
        "--graph-llm-api-key-env",
        "GRAPH_KEY",
        "--graph-llm-model",
        "openai/gpt-4o-mini",
        "--graph-llm-base-url",
        "https://openrouter.ai/api/v1",
        "--graph-llm-timeout-ms",
        "60000",
    ])

    assert args.graph is True
    assert args.graph_build is True
    assert args.graph_weight == 0.4
    assert args.graph_fail_open is True
    assert args.graph_memory_space_mode == "metadata-field"
    assert args.graph_memory_space_field == "tenant_id"
    assert args.graph_owner_id == "bench-owner"
    assert args.graph_llm_api_key_env == "GRAPH_KEY"
    assert args.graph_llm_model == "openai/gpt-4o-mini"
    assert args.graph_llm_base_url == "https://openrouter.ai/api/v1"
    assert args.graph_llm_timeout_ms == 60000


def test_bench_base_command_includes_graph_search_flags(tmp_path):
    parser = personalmem_run.build_parser()
    args = parser.parse_args([
        "search",
        "--dataset",
        str(tmp_path / "data.json"),
        "--graph",
        "--graph-weight",
        "0.4",
        "--graph-fail-open",
        "--graph-memory-space-mode",
        "metadata-field",
        "--graph-memory-space-field",
        "tenant_id",
    ])
    personalmem_run.apply_default_paths(args)

    command = personalmem_run.bench_base_command(args)

    assert "--graph" in command
    assert "--graph-fail-open" in command
    assert "--graph-weight" in command
    assert "0.4" in command
    assert "--graph-memory-space-mode" in command
    assert "metadata-field" in command
    assert "--graph-memory-space-field" in command
    assert "tenant_id" in command
