from __future__ import annotations

import hashlib
import json
from pathlib import Path
import subprocess

import pytest

from common.memory_ab_stage import run_stage


def _writer(calls: list[list[str]], output: Path):
    def run(command, **kwargs):
        calls.append(command)
        output.write_text(f"run {len(calls)}\n", encoding="utf-8")
        return subprocess.CompletedProcess(command, 0)

    return run


def test_stage_rebuilds_when_input_hash_changes(tmp_path):
    source = tmp_path / "source.json"
    output = tmp_path / "output.json"
    source.write_text("first", encoding="utf-8")
    calls = []

    run_stage(
        "prepare",
        ["fake"],
        (output,),
        {"configuration_hash": "c"},
        inputs=(source,),
        runner=_writer(calls, output),
    )
    source.write_text("second", encoding="utf-8")
    run_stage(
        "prepare",
        ["fake"],
        (output,),
        {"configuration_hash": "c"},
        inputs=(source,),
        runner=_writer(calls, output),
    )

    assert len(calls) == 2


def test_stage_reuses_legacy_locomo_complete_manifest(tmp_path):
    source = tmp_path / "source.json"
    output = tmp_path / "output.json"
    source.write_text("source\n", encoding="utf-8")
    output.write_text("prepared\n", encoding="utf-8")
    command = ["fake", "--output", str(output)]
    complete = tmp_path / "stages" / "adapter.complete.json"
    complete.parent.mkdir()
    complete.write_text(
        json.dumps(
            {
                "stage": "adapter",
                "source_hash": "a",
                "configuration_hash": "b",
                "command_hash": hashlib.sha256(
                    json.dumps(
                        command,
                        ensure_ascii=False,
                        sort_keys=True,
                        separators=(",", ":"),
                    ).encode("utf-8")
                ).hexdigest(),
                "inputs": {
                    str(source): hashlib.sha256(source.read_bytes()).hexdigest()
                },
                "outputs": {
                    str(output): hashlib.sha256(output.read_bytes()).hexdigest()
                },
            }
        ),
        encoding="utf-8",
    )
    calls = []

    run_stage(
        "adapter",
        command,
        (output,),
        {"stage": "adapter", "source_hash": "a", "configuration_hash": "b"},
        inputs=(source,),
        runner=_writer(calls, output),
    )

    assert calls == []


def test_stage_cleans_sqlite_sidecars_before_rerun(tmp_path):
    output = tmp_path / "store.sqlite"
    output.write_text("stale", encoding="utf-8")
    Path(str(output) + "-wal").write_text("wal", encoding="utf-8")
    Path(str(output) + "-shm").write_text("shm", encoding="utf-8")

    def rebuild(command, **kwargs):
        assert not output.exists()
        assert not Path(str(output) + "-wal").exists()
        assert not Path(str(output) + "-shm").exists()
        output.write_text("rebuilt", encoding="utf-8")
        return subprocess.CompletedProcess(command, 0)

    run_stage(
        "add",
        ["fake-add"],
        (output,),
        {"configuration_hash": "c"},
        clean_outputs_on_rerun=True,
        runner=rebuild,
    )


def test_stage_requires_inputs_to_exist(tmp_path):
    missing = tmp_path / "missing.json"
    output = tmp_path / "output.json"

    with pytest.raises(ValueError, match="stage prepare is missing inputs"):
        run_stage(
            "prepare",
            ["fake"],
            (output,),
            {"configuration_hash": "c"},
            inputs=(missing,),
            runner=_writer([], output),
        )


def test_stage_completion_records_utc_timestamps_and_duration(tmp_path):
    output = tmp_path / "output.json"

    run_stage(
        "search",
        ["fake"],
        (output,),
        {"configuration_hash": "c"},
        runner=_writer([], output),
    )

    complete = json.loads(
        (tmp_path / "stages" / "search.complete.json").read_text(encoding="utf-8")
    )
    assert complete["started_at"].endswith("+00:00")
    assert complete["finished_at"].endswith("+00:00")
    assert complete["duration_seconds"] >= 0
