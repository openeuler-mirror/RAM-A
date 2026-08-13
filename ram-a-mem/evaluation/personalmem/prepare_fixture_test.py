import json
from pathlib import Path

from personalmem.prepare_fixture import prepare_fixture


def test_prepare_fixture_adds_graph_scope_and_query_filter(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[1] / "fixtures" / "personalmem_sample.json"
    output = tmp_path / "prepared.json"

    assert prepare_fixture(source, output) == 2
    prepared = json.loads(output.read_text(encoding="utf-8"))

    assert prepared["schema_version"] == "benchmark-prepared-v1"
    assert all(
        memory["metadata"]["scope_id"] == "personalmem-sample"
        for memory in prepared["memories"]
    )
    assert all(
        query["filter"] == {"scope_id": "personalmem-sample"}
        for query in prepared["queries"]
    )


def test_prepare_fixture_preserves_fixture_text_and_questions(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[1] / "fixtures" / "personalmem_sample.json"
    output = tmp_path / "prepared.json"

    prepare_fixture(source, output)
    prepared = json.loads(output.read_text(encoding="utf-8"))

    assert [memory["text"] for memory in prepared["memories"]] == [
        "部署前必须检查 health endpoint。",
        "Alice 喜欢简洁的状态更新。",
    ]
    assert [query["text"] for query in prepared["queries"]] == [
        "部署前需要检查什么？",
        "Alice 喜欢什么样的更新？",
    ]
