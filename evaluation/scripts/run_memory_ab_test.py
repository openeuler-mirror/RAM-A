from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

import pytest

import scripts.run_memory_ab as memory_ab_runner
from scripts.run_memory_ab import build_dataset_commands, build_parser, run_pair


class FakeRunner:
    def __init__(self, fail_on: str | None = None) -> None:
        self.fail_on = fail_on
        self.calls: list[tuple[str, list[str]]] = []

    @property
    def stage_names(self) -> list[str]:
        return [stage for stage, _ in self.calls]

    def __call__(self, command, **kwargs):
        stage = kwargs["env"]["RAM_A_MEMORY_AB_STAGE"]
        argv = [str(value) for value in command]
        self.calls.append((stage, argv))
        if stage == self.fail_on:
            raise subprocess.CalledProcessError(1, argv)
        return subprocess.CompletedProcess(argv, 0, stdout="passed", stderr="")


class ArtifactRunner(FakeRunner):
    def __init__(self, args, comparison: dict, history_records=None) -> None:
        super().__init__()
        self.args = args
        self.comparison = comparison
        self.history_records = history_records

    def __call__(self, command, **kwargs):
        completed = super().__call__(command, **kwargs)
        stage = self.calls[-1][0]
        commands = build_dataset_commands(self.args)
        if stage in {"raw", "extracted"}:
            run_dir = commands.pair_dir / stage
            run_dir.mkdir(parents=True, exist_ok=True)
            (run_dir / "config.json").write_text(
                '{"implementation_hash":"impl","promotion_policy_hash":"policy",'
                '"preflight_hash":"preflight","top_k":10}\n',
                encoding="utf-8",
            )
        elif stage == "compare":
            commands.comparison_path.write_text(
                __import__("json").dumps(self.comparison), encoding="utf-8"
            )
            if self.history_records is not None:
                commands.history_artifact_path.write_text(
                    __import__("json").dumps(self.history_records), encoding="utf-8"
                )
        return completed


def _args(dataset: str, phase: str, tmp_path: Path) -> argparse.Namespace:
    source = tmp_path / "prepared.json"
    source.write_text("{}\n", encoding="utf-8")
    policy = tmp_path / "policy.json"
    policy.write_text(
        '{"schema_version":"memory-ab-promotion-v1",'
        '"primary_metric":"qa.overall.accuracy","historical_floor":0}\n',
        encoding="utf-8",
    )
    return argparse.Namespace(
        dataset=dataset,
        phase=phase,
        pair_id="pair-1",
        dataset_file=source,
        output_root=tmp_path / "outputs",
        promotion_policy=policy,
        frozen_config=None,
        history_root=tmp_path / "history",
        python_executable="python",
        embedding="hash",
        extractor_responses=None,
        grounding_responses=None,
        resume=False,
        arm_args=[],
    )


def _write_personalmem_prepared_fixture(path: Path) -> int:
    from personalmem.run import build_prepared_schema_v1

    fixtures = Path(__file__).resolve().parents[1] / "fixtures"
    sample = json.loads(
        (fixtures / "personalmem_sample.json").read_text(encoding="utf-8")
    )
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
    prepared = build_prepared_schema_v1(legacy, "fixture")
    path.write_text(json.dumps(prepared), encoding="utf-8")
    return len(prepared["queries"])


class OfflinePairRunner:
    """Execute arms/comparison, replacing only expensive preflight and live QA."""

    def __init__(self, dataset: str, question_count: int) -> None:
        self.dataset = dataset
        self.question_count = question_count
        self.stages: list[str] = []
        self.executed_pipeline_phases: list[str] = []

    def __call__(self, command, **kwargs):
        stage = kwargs["env"]["RAM_A_MEMORY_AB_STAGE"]
        self.stages.append(stage)
        if stage.startswith("preflight:"):
            return subprocess.CompletedProcess(
                command, 0, stdout="fixture preflight passed", stderr=""
            )

        execution_command = list(command)
        if stage in {"raw", "extracted"} and "--pipeline-phase" in execution_command:
            phase_index = execution_command.index("--pipeline-phase") + 1
            execution_command[phase_index] = "retrieval"
            self.executed_pipeline_phases.append(execution_command[phase_index])

        completed = subprocess.run(
            execution_command,
            cwd=kwargs["cwd"],
            env=kwargs["env"],
            capture_output=True,
            text=True,
            check=False,
        )
        if completed.returncode:
            raise subprocess.CalledProcessError(
                completed.returncode,
                command,
                output=completed.stdout,
                stderr=completed.stderr,
            )
        if stage in {"raw", "extracted"}:
            run_dir = Path(command[command.index("--run-dir") + 1])
            if self.dataset == "personalmem":
                metrics = {
                    "summary": {
                        "total": self.question_count,
                        "correct": 0,
                        "answer_acc": 0.0,
                        "valid_predictions": 0,
                    },
                    "by_question_type": [
                        {
                            "name": "persona",
                            "total": self.question_count,
                            "correct": 0,
                            "accuracy": 0.0,
                        }
                    ],
                    "per_query": [],
                }
                artifact = run_dir / "grade_metrics.json"
            else:
                metrics = {
                    "overall": {
                        "accuracy": 0.0,
                        "total": self.question_count,
                        "correct": 0,
                    },
                    "by_type": {
                        "single-session-user": {
                            "accuracy": 0.0,
                            "total": self.question_count,
                            "correct": 0,
                        }
                    },
                }
                artifact = run_dir / "qa_metrics.json"
            artifact.write_text(json.dumps(metrics), encoding="utf-8")
        return completed


@pytest.mark.parametrize("dataset", ("personalmem", "longmemeval"))
def test_unified_pair_runs_offline_fixture_arms_and_comparison(
    dataset: str,
    tmp_path: Path,
) -> None:
    fixtures = Path(__file__).resolve().parents[1] / "fixtures"
    args = _args(dataset, "pilot", tmp_path)
    if dataset == "personalmem":
        question_count = _write_personalmem_prepared_fixture(args.dataset_file)
        args.arm_args = [
            "--model",
            "hash",
            "--dimensions",
            "32",
            "--top-k",
            "2",
        ]
    else:
        args.dataset_file = fixtures / "longmemeval_sample.json"
        question_count = 1
        args.arm_args = [
            "--embedding-model",
            "hash",
            "--dimensions",
            "32",
            "--retrieval-top-k",
            "2",
            "--qa-top-k",
            "1",
        ]
    args.python_executable = sys.executable
    args.extractor_responses = fixtures / f"{dataset}_memory_extractor_responses.json"
    args.grounding_responses = fixtures / f"{dataset}_memory_grounding_responses.json"
    runner = OfflinePairRunner(dataset, question_count)

    comparison = run_pair(args, runner=runner)
    commands = build_dataset_commands(args)

    assert runner.stages[-3:] == ["raw", "extracted", "compare"]
    assert comparison["complete"] is True
    assert comparison["promotion"]["passed"] is False
    assert commands.comparison_path.is_file()
    assert commands.comparison_html_path.is_file()
    assert not commands.history_artifact_path.exists()
    assert not commands.frozen_path.exists()
    raw_dir = commands.pair_dir / "raw"
    extracted_dir = commands.pair_dir / "extracted"
    for run_dir in (raw_dir, extracted_dir):
        assert (run_dir / "raw_prepared.json").is_file()
        assert (run_dir / "search_results.json").is_file()
    assert (extracted_dir / "extracted_prepared.json").is_file()
    store_name = "store.sqlite" if dataset == "personalmem" else "store.jsonl"
    assert (raw_dir / store_name).is_file()
    assert (extracted_dir / store_name).is_file()
    assert (raw_dir / store_name) != (extracted_dir / store_name)
    from common.memory_ab import file_sha256

    expected_preflight_hash = file_sha256(commands.preflight_path)
    assert {
        memory_ab_runner._read_object(run_dir / "config.json")["preflight_hash"]
        for run_dir in (raw_dir, extracted_dir)
    } == {expected_preflight_hash}
    assert runner.executed_pipeline_phases == ["retrieval", "retrieval"]


@pytest.mark.parametrize("dataset", ("personalmem", "longmemeval"))
def test_unified_pair_rejects_pipeline_phase_override(
    dataset: str,
    tmp_path: Path,
) -> None:
    args = _args(dataset, "pilot", tmp_path)
    args.arm_args = ["--pipeline-phase", "retrieval"]

    with pytest.raises(ValueError, match="pipeline-phase"):
        build_dataset_commands(args)


@pytest.mark.parametrize("dataset", ("personalmem", "longmemeval"))
def test_unified_pair_commands_explicitly_bind_pipeline_phase_all(
    dataset: str,
    tmp_path: Path,
) -> None:
    commands = build_dataset_commands(_args(dataset, "pilot", tmp_path))

    for command in (commands.raw, commands.extracted):
        phase_index = command.argv.index("--pipeline-phase") + 1
        assert command.argv[phase_index] == "all"


@pytest.mark.parametrize(
    "override",
    (
        "--dataset-f=other.json",
        "--frozen-c=other.json",
        "--memory-m=extracted",
        "--pair-i=other-pair",
        "--ph=full",
        "--pipeline-p=retrieval",
        "--prefl=other.json",
        "--promotion-p=other.json",
        "--run-d=other-run",
    ),
)
def test_unified_pair_rejects_governance_option_abbreviations(
    override: str,
    tmp_path: Path,
) -> None:
    args = _args("longmemeval", "pilot", tmp_path)
    args.arm_args = [override]

    with pytest.raises(ValueError, match="unified A/B runner owns"):
        build_dataset_commands(args)


def test_longmemeval_immutable_manifest_binds_pipeline_phase(tmp_path: Path) -> None:
    args = _args("longmemeval", "pilot", tmp_path)
    commands = build_dataset_commands(args)

    manifest = memory_ab_runner._arm_immutable_manifest("longmemeval", commands.raw)

    assert manifest["pipeline_phase"] == "all"


def test_pilot_rejects_external_frozen_config_without_deleting_it(
    tmp_path: Path,
) -> None:
    args = _args("personalmem", "pilot", tmp_path)
    external_frozen = tmp_path / "external-frozen.json"
    external_frozen.write_text("stale\n", encoding="utf-8")
    args.frozen_config = external_frozen

    with pytest.raises(ValueError, match="frozen-config"):
        build_dataset_commands(args)

    assert external_frozen.read_text(encoding="utf-8") == "stale\n"


def test_full_pair_validates_frozen_config_before_running_any_command(
    tmp_path: Path,
) -> None:
    runner = FakeRunner()

    with pytest.raises(ValueError, match="frozen"):
        run_pair(_args("longmemeval", "full", tmp_path), runner=runner)

    assert runner.calls == []


def test_invalid_promotion_policy_is_rejected_before_preflight_or_arms(
    tmp_path: Path,
) -> None:
    args = _args("personalmem", "pilot", tmp_path)
    args.promotion_policy.write_text("{}\n", encoding="utf-8")
    commands = build_dataset_commands(args)
    commands.frozen_path.parent.mkdir(parents=True, exist_ok=True)
    commands.frozen_path.write_text("stale\n", encoding="utf-8")
    runner = FakeRunner()

    with pytest.raises((KeyError, ValueError), match="promotion|schema"):
        run_pair(args, runner=runner)

    assert runner.calls == []
    assert not commands.frozen_path.exists()


def test_full_rejects_frozen_policy_or_implementation_mismatch_before_commands(
    tmp_path: Path,
) -> None:
    args = _args("longmemeval", "full", tmp_path)
    frozen = tmp_path / "frozen.json"
    frozen.write_text(
        '{"promotion_policy_hash":"stale","implementation_hash":"stale"}\n',
        encoding="utf-8",
    )
    args.frozen_config = frozen
    runner = FakeRunner()

    with pytest.raises(ValueError, match="frozen.*(policy|implementation)"):
        run_pair(args, runner=runner)

    assert runner.calls == []


def test_full_requires_complete_dataset_immutable_manifest_before_commands(
    tmp_path: Path,
) -> None:
    args = _args("longmemeval", "full", tmp_path)
    from common.memory_ab import file_sha256

    frozen = tmp_path / "frozen.json"
    frozen.write_text(
        json.dumps(
            {
                "implementation_hash": memory_ab_runner._implementation_hash(
                    "longmemeval"
                ),
                "promotion_policy_hash": file_sha256(args.promotion_policy),
            }
        ),
        encoding="utf-8",
    )
    args.frozen_config = frozen
    runner = FakeRunner()

    with pytest.raises(ValueError, match="frozen configuration mismatch"):
        run_pair(args, runner=runner)

    assert runner.calls == []


def test_full_frozen_gate_precedes_dataset_file_access(tmp_path: Path) -> None:
    from common.memory_ab import file_sha256

    args = _args("longmemeval", "full", tmp_path)
    args.dataset_file = tmp_path / "missing-dataset.json"
    args.frozen_config = tmp_path / "frozen.json"
    args.frozen_config.write_text(
        json.dumps(
            {
                "implementation_hash": memory_ab_runner._implementation_hash(
                    "longmemeval"
                ),
                "promotion_policy_hash": file_sha256(args.promotion_policy),
            }
        ),
        encoding="utf-8",
    )
    runner = FakeRunner()

    with pytest.raises(ValueError, match="frozen configuration mismatch"):
        run_pair(args, runner=runner)

    assert runner.calls == []


def test_comparison_runs_only_after_both_arms_complete(tmp_path: Path) -> None:
    runner = FakeRunner(fail_on="extracted")

    with pytest.raises(subprocess.CalledProcessError):
        run_pair(_args("personalmem", "pilot", tmp_path), runner=runner)

    assert "raw" in runner.stage_names
    assert "extracted" in runner.stage_names
    assert "compare" not in runner.stage_names


def test_preflight_finishes_before_raw_and_extracted_arms(tmp_path: Path) -> None:
    args = _args("personalmem", "pilot", tmp_path)
    commands = build_dataset_commands(args)

    assert commands.raw.stage == "raw"
    assert commands.extracted.stage == "extracted"
    assert "--preflight" in commands.raw.argv
    assert commands.raw.argv[commands.raw.argv.index("--preflight") + 1] == str(
        commands.preflight_path
    )
    assert commands.extracted.argv[
        commands.extracted.argv.index("--preflight") + 1
    ] == str(commands.preflight_path)


def test_locomo_registry_binds_explicit_policy_to_arms_and_comparison(tmp_path: Path) -> None:
    args = _args("locomo", "pilot", tmp_path)

    commands = build_dataset_commands(args)

    assert commands.raw.env_overrides["PROMOTION_POLICY"] == str(
        args.promotion_policy
    )
    assert commands.extracted.env_overrides["PROMOTION_POLICY"] == str(
        args.promotion_policy
    )
    assert commands.compare.argv[commands.compare.argv.index("--policy") + 1] == str(
        args.promotion_policy
    )


def test_cli_forwards_arguments_after_separator_without_forwarding_separator(
    tmp_path: Path,
) -> None:
    source = tmp_path / "prepared.json"
    source.write_text("{}\n", encoding="utf-8")
    policy = tmp_path / "policy.json"
    policy.write_text(
        '{"schema_version":"memory-ab-promotion-v1",'
        '"primary_metric":"qa.overall.accuracy","historical_floor":0}\n',
        encoding="utf-8",
    )
    args = build_parser().parse_args(
        [
            "--dataset",
            "longmemeval",
            "--phase",
            "pilot",
            "--pair-id",
            "pair-1",
            "--dataset-file",
            str(source),
            "--promotion-policy",
            str(policy),
            "--",
            "--max-questions",
            "5",
        ]
    )

    command = build_dataset_commands(args).raw.argv
    assert command[-2:] == ["--max-questions", "5"]
    assert "--" not in command


@pytest.mark.parametrize(
    "module_name",
    ("personalmem.run", "longmemeval.run", "locomo.locomo_run"),
)
def test_dataset_implementation_hash_covers_unified_orchestrator(
    monkeypatch,
    tmp_path: Path,
    module_name: str,
) -> None:
    module = __import__(module_name, fromlist=["implementation_hash"])
    evaluation_root = tmp_path / "evaluation"
    orchestrator = evaluation_root / "scripts" / "run_memory_ab.py"
    orchestrator.parent.mkdir(parents=True)
    orchestrator.write_text("VERSION = 1\n", encoding="utf-8")
    monkeypatch.setattr(module, "EVALUATION_ROOT", evaluation_root)
    if hasattr(module, "PROJECT_ROOT"):
        monkeypatch.setattr(module, "PROJECT_ROOT", tmp_path)

    before = module.implementation_hash()
    orchestrator.write_text("VERSION = 2\n", encoding="utf-8")

    assert module.implementation_hash() != before


def test_pilot_freezes_only_after_passing_comparison(tmp_path: Path) -> None:
    args = _args("personalmem", "pilot", tmp_path)
    commands = build_dataset_commands(args)
    runner = ArtifactRunner(
        args,
        {"complete": True, "promotion": {"passed": True, "reasons": []}},
    )

    run_pair(args, runner=runner)

    assert runner.stage_names[-3:] == ["raw", "extracted", "compare"]
    assert commands.frozen_path.is_file()
    assert __import__("json").loads(
        commands.frozen_path.read_text(encoding="utf-8")
    )["preflight_hash"] == "preflight"
    assert not commands.history_artifact_path.exists()


def test_failed_pilot_does_not_freeze_or_append_history(tmp_path: Path) -> None:
    args = _args("longmemeval", "pilot", tmp_path)
    commands = build_dataset_commands(args)
    commands.frozen_path.parent.mkdir(parents=True, exist_ok=True)
    commands.frozen_path.write_text("stale\n", encoding="utf-8")
    runner = ArtifactRunner(
        args,
        {"complete": True, "promotion": {"passed": False, "reasons": ["floor"]}},
    )

    run_pair(args, runner=runner)

    assert not commands.frozen_path.exists()
    assert not (args.history_root / "records" / "longmemeval.jsonl").exists()


def _history_records(pair_id: str, *, passed: bool) -> list[dict]:
    shared = {
        "schema_version": "memory-ab-history-v1",
        "pair_id": pair_id,
        "dataset": "personalmem",
        "split": "32k",
        "phase": "full",
        "source_hash": "source",
        "code_hash": "code",
        "configuration_hash": "config",
        "preflight_hash": "preflight",
        "policy_hash": "policy",
        "configuration": {},
        "metrics": {},
    }
    return [
        {
            **shared,
            "run_id": "raw-run",
            "memory_mode": "raw",
            "promotion_status": "reference",
            "promotion_reasons": [],
            "artifact_path": "/artifacts/raw",
        },
        {
            **shared,
            "run_id": "extracted-run",
            "memory_mode": "extracted",
            "promotion_status": "passed" if passed else "failed",
            "promotion_reasons": [] if passed else ["fresh_raw_primary"],
            "artifact_path": "/artifacts/extracted",
        },
    ]


def test_complete_failed_full_pair_is_appended_after_comparison(
    monkeypatch,
    tmp_path: Path,
) -> None:
    args = _args("personalmem", "full", tmp_path)
    frozen = tmp_path / "frozen.json"
    args.frozen_config = frozen
    commands = build_dataset_commands(args)
    frozen.write_text(
        json.dumps(
            memory_ab_runner._arm_immutable_manifest("personalmem", commands.raw)
        ),
        encoding="utf-8",
    )
    records = _history_records(args.pair_id, passed=False)
    runner = ArtifactRunner(
        args,
        {"complete": True, "promotion": {"passed": False, "reasons": ["floor"]}},
        records,
    )
    workbook_calls = []
    monkeypatch.setattr(
        memory_ab_runner,
        "build_workbooks",
        lambda root: workbook_calls.append(Path(root)),
        raising=False,
    )

    run_pair(args, runner=runner)

    record_path = args.history_root / "records" / "personalmem.jsonl"
    assert len(record_path.read_text(encoding="utf-8").splitlines()) == 2
    assert workbook_calls == [args.history_root]
