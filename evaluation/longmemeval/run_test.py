import sys
import json
from pathlib import Path

import pytest

import longmemeval.run as longmemeval_run
from longmemeval.preprocess import preprocess
from longmemeval.provenance import (
    build_source_turn_metadata,
    retrieved_source_session_ids,
    retrieved_source_turn_ids,
)
from longmemeval.run import (
    build_extraction_command,
    build_run_paths,
    parse_args,
    validate_experiment_args,
)


FIXTURES = Path(__file__).parents[1] / "fixtures"
LONGMEMEVAL_FIXTURE = FIXTURES / "longmemeval_sample.json"
EXTRACTOR_FIXTURE = FIXTURES / "longmemeval_memory_extractor_responses.json"
GROUNDING_FIXTURE = FIXTURES / "longmemeval_memory_grounding_responses.json"


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


def _parse(monkeypatch, *arguments):
    monkeypatch.setattr(sys, "argv", ["run.py", *arguments])
    return parse_args()


def test_backend_defaults_to_ram_a(monkeypatch):
    monkeypatch.setattr(sys, "argv", ["run.py"])

    args = parse_args()

    assert args.backend == "RAM-A"


def test_backend_accepts_ram_a(monkeypatch):
    monkeypatch.setattr(sys, "argv", ["run.py", "--backend", "RAM-A"])

    args = parse_args()

    assert args.backend == "RAM-A"


def test_cli_accepts_memory_ab_preflight(monkeypatch, tmp_path):
    args = _parse(monkeypatch, "--preflight", str(tmp_path / "preflight.json"))

    assert args.preflight == tmp_path / "preflight.json"


def test_extracted_mode_uses_rust_output_as_indexed_prepared(monkeypatch, tmp_path):
    args = _parse(
        monkeypatch,
        "--memory-mode",
        "extracted",
        "--run-dir",
        str(tmp_path),
    )

    paths = build_run_paths(args)

    assert paths.raw_prepared.name == "raw_prepared.json"
    assert paths.indexed_prepared.name == "extracted_prepared.json"


def test_raw_mode_indexes_raw_prepared(monkeypatch, tmp_path):
    args = _parse(
        monkeypatch,
        "--memory-mode",
        "raw",
        "--run-dir",
        str(tmp_path),
    )

    paths = build_run_paths(args)

    assert paths.raw_prepared.name == "raw_prepared.json"
    assert paths.indexed_prepared == paths.raw_prepared


def test_automatic_run_names_are_distinct_for_raw_and_extracted_arms(
    monkeypatch,
    tmp_path,
):
    monkeypatch.setattr(longmemeval_run, "OUTPUTS_DIR", str(tmp_path))
    raw_args = _parse(
        monkeypatch,
        "--dataset-file",
        "sample.json",
        "--embedding-model",
        "provider/model",
        "--memory-mode",
        "raw",
    )
    extracted_args = _parse(
        monkeypatch,
        "--dataset-file",
        "sample.json",
        "--embedding-model",
        "provider/model",
        "--memory-mode",
        "extracted",
    )

    raw_paths = build_run_paths(raw_args)
    extracted_paths = build_run_paths(extracted_args)

    assert raw_paths.run_dir.name.endswith("_provider_model_sample_raw")
    assert extracted_paths.run_dir.name.endswith(
        "_provider_model_sample_extracted"
    )
    assert raw_paths.run_dir != extracted_paths.run_dir


@pytest.mark.parametrize("memory_mode", ["raw", "extracted"])
def test_automatic_resume_discovers_only_the_requested_memory_arm(
    monkeypatch,
    tmp_path,
    memory_mode,
):
    monkeypatch.setattr(longmemeval_run, "OUTPUTS_DIR", str(tmp_path))
    run_parent = tmp_path / "longmemeval"
    raw_run = run_parent / "2026-01-01T000000_hash_sample_raw"
    extracted_run = run_parent / "2026-01-01T000000_hash_sample_extracted"
    raw_run.mkdir(parents=True)
    extracted_run.mkdir()
    args = _parse(
        monkeypatch,
        "--dataset-file",
        "sample.json",
        "--embedding-model",
        "hash",
        "--memory-mode",
        memory_mode,
        "--resume",
    )

    paths = build_run_paths(args)

    expected = raw_run if memory_mode == "raw" else extracted_run
    assert paths.run_dir == expected


def test_full_mode_requires_frozen_config_before_runner_calls(monkeypatch):
    args = _parse(monkeypatch, "--phase", "full")

    with pytest.raises(ValueError, match="--frozen-config"):
        validate_experiment_args(args)


def test_canonical_experiment_and_pipeline_phases_are_distinct(monkeypatch):
    args = _parse(
        monkeypatch,
        "--phase",
        "full",
        "--pipeline-phase",
        "all",
    )

    assert args.phase == "full"
    assert args.pipeline_phase == "all"


def test_legacy_phase_is_rewritten_with_warning(monkeypatch, capsys):
    args = _parse(monkeypatch, "--phase", "qa")

    assert args.phase == "pilot"
    assert args.pipeline_phase == "qa"
    assert "deprecated" in capsys.readouterr().err.lower()


def test_legacy_phase_conflicts_with_explicit_pipeline_phase(monkeypatch):
    with pytest.raises(SystemExit):
        _parse(
            monkeypatch,
            "--phase=all",
            "--pipeline-phase",
            "retrieval",
        )


def test_fixture_mode_requires_paired_response_maps(monkeypatch, tmp_path):
    args = _parse(
        monkeypatch,
        "--memory-mode",
        "extracted",
        "--extractor-responses",
        str(tmp_path / "extractor.json"),
    )

    with pytest.raises(ValueError, match="both --extractor-responses and"):
        validate_experiment_args(args)


def test_extraction_command_delegates_to_shared_builder(monkeypatch, tmp_path):
    extractor = tmp_path / "extractor.json"
    grounding = tmp_path / "grounding.json"
    args = _parse(
        monkeypatch,
        "--memory-mode",
        "extracted",
        "--run-dir",
        str(tmp_path / "run"),
        "--extractor-responses",
        str(extractor),
        "--grounding-responses",
        str(grounding),
    )
    paths = build_run_paths(args)
    calls = []

    def fake_builder(config, raw, extracted, artifacts):
        calls.append((config, raw, extracted, artifacts))
        return ["shared-memory-pipeline"]

    monkeypatch.setattr(longmemeval_run, "build_memory_pipeline_command", fake_builder)

    command = build_extraction_command(args, paths, "config-hash")

    assert command == ["shared-memory-pipeline"]
    config, raw, extracted, artifacts = calls[0]
    assert config.extractor_responses == extractor
    assert config.grounding_responses == grounding
    assert config.cache_dir == paths.run_dir / "cache" / "memory-pipeline"
    assert config.cache_version == "config-hash"
    assert config.episode_boundary_fields == ("session_id",)
    assert (raw, extracted, artifacts) == (
        paths.raw_prepared,
        paths.indexed_prepared,
        paths.run_dir / "artifacts",
    )


def test_implementation_hash_tracks_memory_pipeline_binary(monkeypatch, tmp_path):
    binary = tmp_path / "memory-pipeline"
    binary.write_bytes(b"first")
    monkeypatch.setenv("MEMORY_PIPELINE_BIN", str(binary))

    first = longmemeval_run.implementation_hash()
    binary.write_bytes(b"second")

    assert longmemeval_run.implementation_hash() != first


def test_full_validation_happens_before_preprocess_or_backend(monkeypatch):
    args = _parse(monkeypatch, "--phase", "full", "--resume")
    calls = []
    monkeypatch.setattr(longmemeval_run, "parse_args", lambda: args)
    monkeypatch.setattr(
        longmemeval_run,
        "latest_run_dir",
        lambda *args: calls.append("latest_run_dir"),
    )
    monkeypatch.setattr(Path, "is_file", lambda self: calls.append("is_file") or True)

    with pytest.raises(ValueError, match="--frozen-config"):
        longmemeval_run.main()

    assert calls == []


def test_full_mode_rejects_frozen_mismatch_before_dataset_access(
    monkeypatch,
    tmp_path,
):
    policy = tmp_path / "promotion-policy.json"
    policy.write_text('{"minimum_accuracy_delta": 0.0}\n', encoding="utf-8")
    frozen = tmp_path / "frozen-config.json"
    args = _parse(
        monkeypatch,
        "--phase",
        "full",
        "--resume",
        "--frozen-config",
        str(frozen),
        "--promotion-policy",
        str(policy),
    )
    implementation_digest = "a" * 64
    policy_digest = longmemeval_run.file_sha256(policy)
    immutable = longmemeval_run.immutable_experiment_manifest(
        args,
        implementation_digest,
        policy_digest,
    )
    immutable["qa_top_k"] = args.qa_top_k + 1
    frozen.write_text(json.dumps(immutable), encoding="utf-8")
    dataset_accesses = []
    monkeypatch.setattr(longmemeval_run, "parse_args", lambda: args)
    monkeypatch.setattr(
        longmemeval_run,
        "implementation_hash",
        lambda: implementation_digest,
    )
    original_is_file = Path.is_file
    run_discoveries = []

    monkeypatch.setattr(
        longmemeval_run,
        "latest_run_dir",
        lambda *args: run_discoveries.append(args) or None,
    )

    def record_dataset_access(path):
        dataset_accesses.append(path)
        return original_is_file(path)

    monkeypatch.setattr(Path, "is_file", record_dataset_access)

    with pytest.raises(ValueError, match="frozen configuration mismatch.*qa_top_k"):
        longmemeval_run.main()

    assert run_discoveries == []
    assert dataset_accesses == []


def test_full_mode_rejects_changed_policy_before_run_or_provider_access(
    monkeypatch,
    tmp_path,
):
    import common.backends
    import longmemeval.preprocess

    dataset = tmp_path / "longmemeval.json"
    policy = tmp_path / "promotion-policy.json"
    policy.write_text('{"minimum_accuracy_delta": 0.0}\n', encoding="utf-8")
    frozen = tmp_path / "frozen-config.json"
    args = _parse(
        monkeypatch,
        "--dataset-file",
        str(dataset),
        "--phase",
        "full",
        "--resume",
        "--frozen-config",
        str(frozen),
        "--promotion-policy",
        str(policy),
    )
    implementation_digest = "a" * 64
    immutable = longmemeval_run.immutable_experiment_manifest(
        args,
        implementation_digest,
        longmemeval_run.file_sha256(policy),
    )
    frozen.write_text(json.dumps(immutable), encoding="utf-8")
    policy.write_text('{"minimum_accuracy_delta": 0.1}\n', encoding="utf-8")
    calls = []
    monkeypatch.setattr(longmemeval_run, "parse_args", lambda: args)
    monkeypatch.setattr(
        longmemeval_run,
        "implementation_hash",
        lambda: implementation_digest,
    )
    monkeypatch.setattr(
        longmemeval_run,
        "latest_run_dir",
        lambda *args: calls.append("run discovery"),
    )
    monkeypatch.setattr(
        longmemeval_run,
        "_write_json_atomic",
        lambda *args: calls.append("run write"),
    )
    monkeypatch.setattr(
        longmemeval_run,
        "ensure_run_mode",
        lambda *args: calls.append("run write"),
    )
    monkeypatch.setattr(
        longmemeval.preprocess,
        "preprocess",
        lambda *args, **kwargs: calls.append("dataset preprocess"),
    )
    monkeypatch.setattr(
        common.backends,
        "create_backend",
        lambda *args, **kwargs: calls.append("provider construction"),
    )
    original_is_file = Path.is_file

    def record_dataset_access(path):
        if path == dataset:
            calls.append("dataset access")
        return original_is_file(path)

    monkeypatch.setattr(Path, "is_file", record_dataset_access)

    with pytest.raises(
        ValueError,
        match="frozen configuration mismatch.*promotion_policy_hash",
    ):
        longmemeval_run.main()

    assert calls == []


def test_extracted_arm_routes_raw_and_indexed_prepared_to_correct_consumers(
    monkeypatch,
    tmp_path,
):
    dataset = tmp_path / "longmemeval.json"
    dataset.write_text("[]\n", encoding="utf-8")
    run_dir = tmp_path / "run"
    preflight = tmp_path / "preflight.json"
    _write_preflight(
        preflight,
        "longmemeval",
        longmemeval_run.implementation_hash(),
    )
    args = _parse(
        monkeypatch,
        "--dataset-file",
        str(dataset),
        "--run-dir",
        str(run_dir),
        "--memory-mode",
        "extracted",
        "--pipeline-phase",
        "all",
        "--pair-id",
        "pair-42",
        "--extractor-responses",
        str(tmp_path / "extractor.json"),
        "--grounding-responses",
        str(tmp_path / "grounding.json"),
        "--preflight",
        str(preflight),
    )
    calls = []
    prepared = {
        "schema_version": "benchmark-prepared-v1",
        "dataset": {"name": "longmemeval"},
        "memories": [],
        "queries": [],
    }

    def fake_preprocess(source, output, max_items=None):
        calls.append(("preprocess", Path(source), Path(output)))
        Path(output).write_text(json.dumps(prepared), encoding="utf-8")

    def fake_stage(name, command, outputs, manifest, **kwargs):
        calls.append((name, tuple(Path(path) for path in outputs), kwargs))
        if name == "extract":
            Path(outputs[0]).write_text(json.dumps(prepared), encoding="utf-8")
        for output in outputs[1:]:
            Path(output).parent.mkdir(parents=True, exist_ok=True)
            Path(output).write_text("{}\n", encoding="utf-8")

    class FakeBackend:
        persists_local_store = False

        def add(self, prepared_path):
            calls.append(("add", Path(prepared_path)))

        def search(self, prepared_path, output_path):
            calls.append(("search", Path(prepared_path)))
            Path(output_path).write_text("[]\n", encoding="utf-8")

    def fake_retrieval(search, source, output, prepared_path=None):
        calls.append(("retrieval", Path(prepared_path)))
        Path(output).write_text("{}\n", encoding="utf-8")
        return dict(longmemeval_run._EMPTY_METRICS)

    def fake_qa(**kwargs):
        calls.append(("qa", Path(kwargs["prepared_path"])))
        Path(kwargs["output_results_path"]).write_text("[]\n", encoding="utf-8")
        Path(kwargs["output_metrics_path"]).write_text("{}\n", encoding="utf-8")
        return {"overall": {"accuracy": 0.0}}

    import common.backends
    import longmemeval.eval_qa
    import longmemeval.eval_retrieval
    import longmemeval.preprocess
    import longmemeval.report

    monkeypatch.setattr(longmemeval_run, "parse_args", lambda: args)
    monkeypatch.setattr(longmemeval_run, "run_stage", fake_stage, raising=False)
    monkeypatch.setattr(longmemeval.preprocess, "preprocess", fake_preprocess)
    monkeypatch.setattr(common.backends, "create_backend", lambda config: FakeBackend())
    monkeypatch.setattr(longmemeval.eval_retrieval, "load_and_evaluate", fake_retrieval)
    monkeypatch.setattr(longmemeval.eval_qa, "load_and_evaluate_qa", fake_qa)
    monkeypatch.setattr(
        longmemeval.report,
        "generate_longmemeval_report",
        lambda *args, **kwargs: None,
    )
    monkeypatch.setattr(
        longmemeval.report,
        "generate_longmemeval_error_report",
        lambda *args, **kwargs: None,
    )

    longmemeval_run.main()

    raw = run_dir / "raw_prepared.json"
    indexed = run_dir / "extracted_prepared.json"
    assert ("preprocess", dataset, raw) in calls
    assert ("add", indexed) in calls
    assert ("search", indexed) in calls
    assert ("retrieval", raw) in calls
    assert ("qa", indexed) in calls
    extract_call = next(call for call in calls if call[0] == "extract")
    assert extract_call[2]["inputs"] == (
        raw,
        tmp_path / "extractor.json",
        tmp_path / "grounding.json",
    )
    config = json.loads((run_dir / "config.json").read_text(encoding="utf-8"))
    assert config["memory_mode"] == "extracted"
    assert config["pair_id"] == "pair-42"
    assert len(config["source_hash"]) == 64
    assert len(config["configuration_hash"]) == 64
    assert len(config["implementation_hash"]) == 64
    assert config["preflight_hash"] == longmemeval_run.file_sha256(preflight)
    assert config["extraction_cache_dir"] == str(
        run_dir / "cache" / "memory-pipeline"
    )
    assert config["extraction_cache_version"] == config["configuration_hash"]
    run_meta = json.loads((run_dir / "run_meta.json").read_text(encoding="utf-8"))
    assert run_meta["phase"] == "pilot"
    assert run_meta["pipeline_phase"] == "all"


def test_offline_extracted_arm_recovers_gold_source_turn_and_session(
    monkeypatch,
    tmp_path,
):
    run_dir = tmp_path / "run"
    raw_prepared = tmp_path / "derived_raw_prepared.json"
    preprocess(str(LONGMEMEVAL_FIXTURE), str(raw_prepared))
    raw = json.loads(raw_prepared.read_text(encoding="utf-8"))
    monkeypatch.delenv("OPENROUTER_API_KEY", raising=False)
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "run.py",
            "--dataset-file",
            str(LONGMEMEVAL_FIXTURE),
            "--run-dir",
            str(run_dir),
            "--memory-mode",
            "extracted",
            "--pipeline-phase",
            "retrieval",
            "--pair-id",
            "offline-longmemeval",
            "--extractor-responses",
            str(EXTRACTOR_FIXTURE),
            "--grounding-responses",
            str(GROUNDING_FIXTURE),
            "--embedding",
            "hash",
            "--embedding-model",
            "hash",
            "--dimensions",
            "32",
            "--retrieval-top-k",
            "2",
            "--qa-top-k",
            "1",
        ],
    )

    longmemeval_run.main()

    indexed = json.loads(
        (run_dir / "extracted_prepared.json").read_text(encoding="utf-8")
    )
    assert indexed["queries"] == raw["queries"]
    assert indexed["memories"]
    assert all(
        memory["metadata"]["memory_kind"] == "extracted_memory"
        for memory in indexed["memories"]
    )
    search_results = json.loads(
        (run_dir / "search_results.json").read_text(encoding="utf-8")
    )
    source_metadata = build_source_turn_metadata(raw)
    gold_turn = raw["queries"][0]["task"]["gold_turn_ids"][0]
    assert gold_turn in retrieved_source_turn_ids(search_results[0])
    assert "session-beta" in retrieved_source_session_ids(
        search_results[0],
        source_metadata,
    )
