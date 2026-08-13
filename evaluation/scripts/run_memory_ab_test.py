from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

import pytest

import scripts.run_memory_ab as memory_ab_runner
from scripts.run_memory_ab import build_dataset_commands, build_parser, run_pair, run_single


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
    def __init__(self, args, comparison: dict) -> None:
        super().__init__()
        self.args = args
        self.comparison = comparison

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
        mode="strict",
        pair_id="pair-1",
        dataset_file=source,
        output_root=tmp_path / "outputs",
        promotion_policy=policy,
        python_executable="python",
        extractor_responses=None,
        grounding_responses=None,
        resume=False,
        arm_args=[],
        execution="ab",
        memory_mode=None,
    )


def test_normal_mode_forwards_mode_without_preflight_gate(
    tmp_path: Path,
) -> None:
    args = _args("longmemeval", "full", tmp_path)
    args.mode = "normal"

    commands = build_dataset_commands(args)

    assert "--mode" in commands.raw.argv
    assert commands.raw.argv[commands.raw.argv.index("--mode") + 1] == "normal"
    assert "--mode" in commands.extracted.argv
    assert commands.extracted.argv[commands.extracted.argv.index("--mode") + 1] == "normal"


def test_normal_mode_skips_heavy_preflight_gate(tmp_path: Path) -> None:
    args = _args("longmemeval", "full", tmp_path)
    args.mode = "normal"
    args.promotion_policy = None
    runner = ArtifactRunner(args, {"complete": False})

    comparison = run_pair(args, runner=runner)

    assert comparison == {"complete": False}
    assert runner.stage_names == ["raw", "extracted", "compare"]


@pytest.mark.parametrize("memory_mode", ("raw", "extracted"))
def test_single_mode_runs_only_selected_arm(tmp_path: Path, memory_mode: str) -> None:
    args = _args("longmemeval", "full", tmp_path)
    args.mode = "normal"
    args.execution = "single"
    args.memory_mode = memory_mode
    args.promotion_policy = None
    runner = FakeRunner()

    run_single(args, runner=runner)

    assert runner.stage_names == [memory_mode]


def test_single_mode_rejects_promotion_policy(tmp_path: Path) -> None:
    args = _args("longmemeval", "full", tmp_path)
    args.mode = "normal"
    args.execution = "single"
    args.memory_mode = "raw"
    runner = FakeRunner()

    with pytest.raises(ValueError, match="only valid in strict mode"):
        run_single(args, runner=runner)

    assert runner.calls == []


def test_ab_mode_rejects_memory_mode(tmp_path: Path) -> None:
    args = _args("longmemeval", "full", tmp_path)
    args.memory_mode = "raw"

    with pytest.raises(ValueError, match="ab execution does not accept memory_mode"):
        memory_ab_runner._validate_selectors(args)


@pytest.mark.parametrize("dataset", ("personalmem", "longmemeval"))
def test_resume_is_forwarded_to_both_dataset_arms(
    dataset: str,
    tmp_path: Path,
) -> None:
    args = _args(dataset, "full", tmp_path)
    args.resume = True

    commands = build_dataset_commands(args)

    assert "--resume" in commands.raw.argv
    assert "--resume" in commands.extracted.argv


def test_locomo_resume_is_managed_inside_the_dataset_runner(tmp_path: Path) -> None:
    args = _args("locomo", "full", tmp_path)
    args.resume = True

    commands = build_dataset_commands(args)

    assert "--resume" not in commands.raw.argv
    assert "--resume" not in commands.extracted.argv


def _write_personalmem_prepared_fixture(path: Path) -> int:
    fixtures = Path(__file__).resolve().parents[1] / "fixtures"
    from personalmem.prepare_fixture import prepare_fixture

    return prepare_fixture(fixtures / "personalmem_sample.json", path)


@pytest.mark.parametrize(
    ("field", "value"),
    (
        ("dataset", "../locomo"),
        ("phase", "../full"),
        ("pair_id", "../pair-1"),
    ),
)
def test_selectors_reject_path_traversal_components(
    tmp_path: Path, field: str, value: str
) -> None:
    args = _args("locomo", "full", tmp_path)
    setattr(args, field, value)

    with pytest.raises(ValueError):
        memory_ab_runner._validate_selectors(args)


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
    args = _args(dataset, "full", tmp_path)
    if dataset == "personalmem":
        question_count = _write_personalmem_prepared_fixture(args.dataset_file)
        args.arm_args = [
            "--embedding",
            "hash",
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
        ]
    args.python_executable = sys.executable
    args.extractor_responses = fixtures / f"{dataset}_memory_extractor_responses.json"
    args.grounding_responses = fixtures / f"{dataset}_memory_grounding_responses.json"
    runner = OfflinePairRunner(dataset, question_count)

    comparison = run_pair(args, runner=runner)
    commands = build_dataset_commands(args)

    assert runner.stages[-3:] == ["raw", "extracted", "compare"]
    assert comparison["complete"] is False
    assert comparison["promotion"]["passed"] is False
    assert commands.comparison_path.is_file()
    assert commands.comparison_html_path.is_file()
    assert not commands.history_artifact_path.exists()
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
    args = _args(dataset, "full", tmp_path)
    args.arm_args = ["--pipeline-phase", "retrieval"]

    with pytest.raises(ValueError, match="pipeline-phase"):
        build_dataset_commands(args)


@pytest.mark.parametrize("dataset", ("personalmem", "longmemeval"))
def test_unified_pair_commands_explicitly_bind_pipeline_phase_all(
    dataset: str,
    tmp_path: Path,
) -> None:
    commands = build_dataset_commands(_args(dataset, "full", tmp_path))

    for command in (commands.raw, commands.extracted):
        phase_index = command.argv.index("--pipeline-phase") + 1
        assert command.argv[phase_index] == "all"


@pytest.mark.parametrize(
    "override",
    (
        "--dataset-f=other.json",
        "--extractor-r=extractor.json",
        "--grounding-r=grounding.json",
        "--memory-m=extracted",
        "--mode=strict",
        "--pair-i=other-pair",
        "--ph=full",
        "--pipeline-p=retrieval",
        "--prefl=other.json",
        "--promotion-p=other.json",
        "--run-d=other-run",
        "--res",
    ),
)
def test_unified_pair_rejects_governance_option_abbreviations(
    override: str,
    tmp_path: Path,
) -> None:
    args = _args("longmemeval", "full", tmp_path)
    args.mode = "normal"
    args.arm_args = [override]

    with pytest.raises(ValueError, match="unified A/B runner owns"):
        build_dataset_commands(args)


def test_full_pair_runs_without_config_snapshot(
    tmp_path: Path,
) -> None:
    args = _args("longmemeval", "full", tmp_path)
    args.mode = "normal"
    args.promotion_policy = None
    runner = ArtifactRunner(
        args,
        {"complete": True},
    )

    comparison = run_pair(args, runner=runner)

    assert comparison == {"complete": True}
    assert runner.stage_names == ["raw", "extracted", "compare"]


def test_invalid_promotion_policy_is_rejected_before_preflight_or_arms(
    tmp_path: Path,
) -> None:
    args = _args("personalmem", "full", tmp_path)
    args.promotion_policy.write_text("{}\n", encoding="utf-8")
    commands = build_dataset_commands(args)
    runner = FakeRunner()

    with pytest.raises((KeyError, ValueError), match="promotion|schema"):
        run_pair(args, runner=runner)

    assert runner.calls == []
def test_comparison_runs_only_after_both_arms_complete(tmp_path: Path) -> None:
    runner = FakeRunner(fail_on="extracted")

    with pytest.raises(subprocess.CalledProcessError):
        run_pair(_args("personalmem", "full", tmp_path), runner=runner)

    assert "raw" in runner.stage_names
    assert "extracted" in runner.stage_names
    assert "compare" not in runner.stage_names


def test_preflight_finishes_before_raw_and_extracted_arms(tmp_path: Path) -> None:
    args = _args("personalmem", "full", tmp_path)
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
    args = _args("locomo", "full", tmp_path)

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
            "full",
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
