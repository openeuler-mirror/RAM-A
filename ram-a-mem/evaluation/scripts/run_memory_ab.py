#!/usr/bin/env python3
"""Run one governed raw/extracted memory A/B pair."""

from __future__ import annotations

import argparse
from dataclasses import dataclass, field
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import subprocess
import sys
import time
from typing import Callable, Sequence


EVALUATION_ROOT = Path(__file__).resolve().parents[1]
PROJECT_ROOT = EVALUATION_ROOT.parent
sys.path.insert(0, str(PROJECT_ROOT))
sys.path.insert(0, str(EVALUATION_ROOT))
PREFLIGHT_SUITES = (
    "python_evaluation",
    "rust_workspace",
    "rust_clippy",
    "diff_check",
)


@dataclass(frozen=True)
class CommandSpec:
    stage: str
    argv: list[str]
    cwd: Path = EVALUATION_ROOT
    env_overrides: dict[str, str] = field(default_factory=dict)


@dataclass(frozen=True)
class DatasetCommands:
    pair_dir: Path
    preflight_path: Path
    comparison_path: Path
    comparison_html_path: Path
    history_artifact_path: Path
    raw: CommandSpec
    extracted: CommandSpec
    compare: CommandSpec


def build_dataset_commands(args: argparse.Namespace) -> DatasetCommands:
    _validate_forwarded_arm_args(getattr(args, "arm_args", []))
    pair_dir = (
        Path(args.output_root)
        / str(args.dataset)
        / str(args.phase)
        / str(args.pair_id)
    )
    raw_dir = pair_dir / "raw"
    extracted_dir = pair_dir / "extracted"
    preflight_path = pair_dir / "preflight.json"
    comparison_path = pair_dir / "comparison.json"
    comparison_html = pair_dir / "comparison.html"
    history_artifact = pair_dir / "history_record.json"
    python = str(args.python_executable)

    common = [
        "--phase",
        str(args.phase),
        "--pair-id",
        str(args.pair_id),
        "--mode",
        str(args.mode),
    ]
    if args.mode == "strict":
        common.extend(["--preflight", str(preflight_path)])
    if args.promotion_policy is not None:
        common.extend(["--promotion-policy", str(args.promotion_policy)])
    if getattr(args, "resume", False):
        common.append("--resume")
    fixtures: list[str] = []
    if getattr(args, "extractor_responses", None) is not None:
        fixtures.extend(
            ["--extractor-responses", str(args.extractor_responses)]
        )
    if getattr(args, "grounding_responses", None) is not None:
        fixtures.extend(
            ["--grounding-responses", str(args.grounding_responses)]
        )
    forwarded = [str(value) for value in getattr(args, "arm_args", [])]
    if forwarded[:1] == ["--"]:
        forwarded = forwarded[1:]

    if args.dataset == "personalmem":
        def persona_arm(mode: str, run_dir: Path) -> CommandSpec:
            return CommandSpec(
                mode if mode == "raw" else "extracted",
                [
                    python,
                    "-m",
                    "personalmem.run",
                    "memory-ab-pipeline",
                    "--dataset",
                    str(args.dataset_file),
                    "--run-dir",
                    str(run_dir),
                    "--memory-mode",
                    mode,
                    "--pipeline-phase",
                    "all",
                    *common,
                    *fixtures,
                    *forwarded,
                ],
            )

        raw = persona_arm("raw", raw_dir)
        extracted = persona_arm("extracted", extracted_dir)
        compare_argv = [
            python,
            "-m",
            "personalmem.compare",
            "--raw-dir",
            str(raw_dir),
            "--treatment-dir",
            str(extracted_dir),
            "--output-json",
            str(comparison_path),
            "--history-record",
            str(history_artifact),
        ]
        if args.promotion_policy is not None:
            compare_argv.extend(["--policy", str(args.promotion_policy)])
        compare = CommandSpec("compare", compare_argv)
    elif args.dataset == "longmemeval":
        def lme_arm(mode: str, run_dir: Path) -> CommandSpec:
            return CommandSpec(
                mode if mode == "raw" else "extracted",
                [
                    python,
                    "-m",
                    "longmemeval.run",
                    "--dataset-file",
                    str(args.dataset_file),
                    "--run-dir",
                    str(run_dir),
                    "--memory-mode",
                    mode,
                    "--pipeline-phase",
                    "all",
                    *common,
                    *fixtures,
                    *forwarded,
                ],
            )

        raw = lme_arm("raw", raw_dir)
        extracted = lme_arm("extracted", extracted_dir)
        compare_argv = [
            python,
            "-m",
            "longmemeval.compare",
            "--raw-dir",
            str(raw_dir),
            "--treatment-dir",
            str(extracted_dir),
            "--output-json",
            str(comparison_path),
            "--history-record",
            str(history_artifact),
        ]
        if args.promotion_policy is not None:
            compare_argv.extend(["--policy", str(args.promotion_policy)])
        compare = CommandSpec("compare", compare_argv)
    elif args.dataset == "locomo":
        base_env = {
            "PAIR_ID": str(args.pair_id),
            "RUN_MODE": str(args.mode),
        }
        if args.mode == "strict":
            base_env["PREFLIGHT_PATH"] = str(preflight_path)
        if args.promotion_policy is not None:
            base_env["PROMOTION_POLICY"] = str(args.promotion_policy)

        def locomo_arm(mode: str, run_dir: Path) -> CommandSpec:
            return CommandSpec(
                mode if mode == "raw" else "extracted",
                [
                    python,
                    "locomo/locomo_run.py",
                    "--phase",
                    str(args.phase),
                    "--dataset",
                    str(args.dataset_file),
                    "--run-dir",
                    str(run_dir),
                    *forwarded,
                ],
                env_overrides={**base_env, "MEMORY_MODE": mode},
            )

        raw = locomo_arm("raw", raw_dir)
        extracted = locomo_arm("extracted", extracted_dir)
        compare_argv = [
            python,
            "locomo/locomo_compare.py",
            "--phase",
            str(args.phase),
            "--raw-dir",
            str(raw_dir),
            "--treatment-dir",
            str(extracted_dir),
            "--output-json",
            str(comparison_path),
            "--html-report",
            str(comparison_html),
        ]
        if args.promotion_policy is not None:
            compare_argv.extend(["--policy", str(args.promotion_policy)])
        compare = CommandSpec("compare", compare_argv)
    else:
        raise ValueError(f"unsupported memory A/B dataset: {args.dataset}")

    return DatasetCommands(
        pair_dir=pair_dir,
        preflight_path=preflight_path,
        comparison_path=comparison_path,
        comparison_html_path=comparison_html,
        history_artifact_path=history_artifact,
        raw=raw,
        extracted=extracted,
        compare=compare,
    )


def run_pair(
    args: argparse.Namespace,
    runner: Callable[..., subprocess.CompletedProcess] = subprocess.run,
) -> dict:
    _validate_selectors(args)
    commands = build_dataset_commands(args)
    _validate_policy(args)
    _validate_runtime_inputs(args)

    commands.pair_dir.mkdir(parents=True, exist_ok=True)
    if args.mode == "strict":
        _run_preflight(args, commands.preflight_path, runner)
    _invoke(commands.raw, runner)
    _invoke(commands.extracted, runner)
    _invoke(commands.compare, runner)

    if not commands.comparison_path.is_file():
        raise ValueError("comparison command did not write comparison.json")
    comparison = _read_object(commands.comparison_path)
    if args.dataset != "locomo":
        _write_comparison_html(commands.comparison_html_path, comparison)
    return comparison


def run_single(
    args: argparse.Namespace,
    runner: Callable[..., subprocess.CompletedProcess] = subprocess.run,
) -> None:
    _validate_selectors(args)
    if args.mode != "normal":
        raise ValueError("single execution only supports normal mode")
    _validate_policy(args)
    commands = build_dataset_commands(args)
    _validate_runtime_inputs(args)
    commands.pair_dir.mkdir(parents=True, exist_ok=True)
    _invoke(commands.raw if args.memory_mode == "raw" else commands.extracted, runner)


def _validate_selectors(args: argparse.Namespace) -> None:
    if args.phase != "full":
        raise ValueError("phase must be full")
    if args.dataset not in {"personalmem", "longmemeval", "locomo"}:
        raise ValueError(f"unsupported memory A/B dataset: {args.dataset}")
    if args.mode not in {"normal", "strict"}:
        raise ValueError("mode must be normal or strict")
    execution = getattr(args, "execution", "ab")
    if execution not in {"single", "ab"}:
        raise ValueError("execution must be single or ab")
    if execution == "single" and getattr(args, "memory_mode", None) not in {
        "raw",
        "extracted",
    }:
        raise ValueError("single execution requires memory_mode raw or extracted")
    if execution == "ab" and getattr(args, "memory_mode", None) is not None:
        raise ValueError("ab execution does not accept memory_mode")
    from common.run_artifacts import safe_slug

    safe_slug(str(args.pair_id))


def _validate_policy(args: argparse.Namespace) -> None:
    if args.promotion_policy is None:
        if args.mode == "normal":
            return
        raise ValueError("promotion policy file is required in strict mode")
    if args.mode == "normal":
        raise ValueError("promotion policy is only valid in strict mode")
    if not Path(args.promotion_policy).is_file():
        raise ValueError("promotion policy file is required")

    policy = _read_object(Path(args.promotion_policy))
    if args.dataset in {"personalmem", "longmemeval"}:
        from common.memory_ab_compare import PromotionPolicy

        PromotionPolicy.from_dict(policy)
    else:
        from locomo.locomo_compare import promotion_policy_manifest

        if policy != promotion_policy_manifest():
            raise ValueError("LoCoMo promotion policy does not match the current policy")


def _validate_runtime_inputs(args: argparse.Namespace) -> None:
    if not Path(args.dataset_file).is_file():
        raise ValueError(f"dataset does not exist: {args.dataset_file}")
    fixtures = (
        getattr(args, "extractor_responses", None),
        getattr(args, "grounding_responses", None),
    )
    if any(fixtures) and not all(fixtures):
        raise ValueError("both extractor and grounding response fixtures are required")
    for fixture in fixtures:
        if fixture is not None and not Path(fixture).is_file():
            raise ValueError(f"response fixture does not exist: {fixture}")


def _validate_forwarded_arm_args(values: Sequence[str]) -> None:
    protected = {
        "--dataset",
        "--dataset-file",
        "--extractor-responses",
        "--grounding-responses",
        "--indexed-dataset",
        "--memory-mode",
        "--mode",
        "--pair-id",
        "--phase",
        "--pipeline-phase",
        "--preflight",
        "--promotion-policy",
        "--run-dir",
        "--resume",
    }
    for value in values:
        if value == "--":
            continue
        option = str(value).split("=", 1)[0]
        if option in protected or any(
            option.startswith("--") and protected_option.startswith(option)
            for protected_option in protected
        ):
            raise ValueError(
                f"unified A/B runner owns {option}; do not forward it to an arm"
            )


def _run_preflight(
    args: argparse.Namespace,
    output: Path,
    runner: Callable[..., subprocess.CompletedProcess],
) -> dict:
    expected_hash = _implementation_hash(str(args.dataset))
    commands = (
        (
            "python_evaluation",
            [str(args.python_executable), "-m", "pytest", "-q"],
            EVALUATION_ROOT,
        ),
        ("rust_workspace", ["cargo", "test", "--workspace"], PROJECT_ROOT),
        (
            "rust_clippy",
            ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
            PROJECT_ROOT,
        ),
        ("diff_check", ["git", "diff", "--check"], PROJECT_ROOT),
    )
    suites = []
    started_at = _now()
    for name, command, cwd in commands:
        started = time.monotonic()
        env = dict(os.environ)
        env["RAM_A_MEMORY_AB_STAGE"] = f"preflight:{name}"
        completed = runner(
            command,
            cwd=cwd,
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )
        suites.append(
            {
                "name": name,
                "command": command,
                "cwd": str(cwd),
                "exit_code": int(completed.returncode),
                "duration_seconds": round(time.monotonic() - started, 3),
                "stdout_tail": str(completed.stdout or "")[-4000:],
                "stderr_tail": str(completed.stderr or "")[-4000:],
            }
        )
    report = {
        "schema_version": "memory-ab-preflight-v1",
        "dataset": str(args.dataset),
        "started_at": started_at,
        "finished_at": _now(),
        "implementation_hash": expected_hash,
        "passed": all(item["exit_code"] == 0 for item in suites),
        "suites": suites,
    }
    _write_json_atomic(output, report)
    if not report["passed"]:
        raise RuntimeError("memory A/B preflight did not pass")
    return report


def _implementation_hash(dataset: str) -> str:
    if dataset == "personalmem":
        from personalmem.run import implementation_hash
    elif dataset == "longmemeval":
        from longmemeval.run import implementation_hash
    else:
        from locomo.locomo_run import implementation_hash
    return implementation_hash()


def _invoke(
    spec: CommandSpec,
    runner: Callable[..., subprocess.CompletedProcess],
) -> subprocess.CompletedProcess:
    env = dict(os.environ)
    env.update(spec.env_overrides)
    env["RAM_A_MEMORY_AB_STAGE"] = spec.stage
    return runner(spec.argv, cwd=spec.cwd, env=env, check=True)


def _read_object(path: Path) -> dict:
    value = json.loads(Path(path).read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError(f"{path} must contain a JSON object")
    return value


def _write_json_atomic(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(
        json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n",
        encoding="utf-8",
    )
    temporary.replace(path)


def _write_comparison_html(path: Path, comparison: dict) -> None:
    from common.report import html_escape

    payload = html_escape(json.dumps(comparison, ensure_ascii=False, indent=2))
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        "<!doctype html><html><head><meta charset=\"utf-8\">"
        "<title>Memory A/B comparison</title></head><body>"
        f"<h1>Memory A/B comparison</h1><pre>{payload}</pre></body></html>\n",
        encoding="utf-8",
    )


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Run a governed RAM-A raw/extracted memory A/B pair."
    )
    parser.add_argument(
        "--dataset", choices=("personalmem", "longmemeval", "locomo"), required=True
    )
    parser.add_argument("--phase", choices=("full",), required=True)
    parser.add_argument(
        "--mode",
        choices=("normal", "strict"),
        default="normal",
        help="normal records the run; strict enables governance checks",
    )
    parser.add_argument(
        "--execution",
        choices=("single", "ab"),
        default="ab",
        help="single runs one memory arm; ab runs raw and extracted arms",
    )
    parser.add_argument(
        "--memory-mode",
        choices=("raw", "extracted"),
        help="memory representation for single execution",
    )
    parser.add_argument("--pair-id", required=True)
    parser.add_argument("--dataset-file", type=Path, required=True)
    parser.add_argument(
        "--output-root", type=Path, default=EVALUATION_ROOT / "outputs" / "memory-ab"
    )
    parser.add_argument("--promotion-policy", type=Path)
    parser.add_argument("--python-executable", default=sys.executable)
    parser.add_argument("--extractor-responses", type=Path)
    parser.add_argument("--grounding-responses", type=Path)
    parser.add_argument("--resume", action="store_true")
    parser.add_argument("arm_args", nargs=argparse.REMAINDER)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if args.execution == "single":
        run_single(args)
    else:
        run_pair(args)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
