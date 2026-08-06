from common.graph_context import GraphFactContextRenderer, enrich_text_with_graph_facts


def _metadata():
    return {
        "graph_facts": [
            {
                "fact_id": "fact-duplicate",
                "predicate": "LIKES",
                "fact_text": "Alex likes jazz.",
                "valid_from_ms": 1_704_067_200_000,
                "recorded_at_ms": 1_704_153_600_000,
            },
            {
                "fact_id": "fact-1",
                "predicate": "LIKES",
                "fact_text": "Alex likes jazz.",
            },
            {
                "fact_id": "fact-2",
                "predicate": "VISITED",
                "fact_text": "Alex visited Kyoto.",
            },
            {
                "fact_id": "fact-3",
                "predicate": "WORKS_AT",
                "fact_text": "Alex works at Acme.",
            },
            {
                "fact_id": "fact-4",
                "predicate": "LIVES_IN",
                "fact_text": "Alex lives in Seattle.",
            },
        ]
    }


def test_graph_fact_rendering_can_be_disabled_without_changing_text():
    text = "Alex discussed music."

    assert enrich_text_with_graph_facts(text, _metadata(), max_facts=0) == text


def test_graph_fact_rendering_is_bounded_deduplicated_and_temporal():
    rendered = enrich_text_with_graph_facts(
        "Alex discussed music.",
        _metadata(),
        max_facts=3,
    )

    assert rendered.startswith("Alex discussed music.\nMatched graph facts:")
    assert "[LIKES] Alex likes jazz. [valid from 2024-01-01T00:00:00Z]" in rendered
    assert rendered.count("Alex likes jazz.") == 1
    assert "[VISITED] Alex visited Kyoto." in rendered
    assert "[WORKS_AT] Alex works at Acme." in rendered
    assert "Alex lives in Seattle." not in rendered
    assert "recorded" not in rendered


def test_graph_fact_already_present_in_memory_is_not_repeated():
    text = "Alex likes jazz."

    assert (
        enrich_text_with_graph_facts(
            text,
            {"graph_facts": [_metadata()["graph_facts"][0]]},
            max_facts=3,
        )
        == text
    )


def test_graph_fact_source_match_respects_token_boundaries():
    rendered = enrich_text_with_graph_facts(
        "The party starts tonight.",
        {
            "graph_facts": [
                {
                    "fact_id": "fact-art",
                    "predicate": "LIKES",
                    "fact_text": "art",
                }
            ]
        },
        max_facts=3,
    )

    assert "[LIKES] art" in rendered


def test_graph_fact_source_match_handles_unsegmented_cjk_text():
    text = "Caroline正在研究收养机构。"

    assert (
        enrich_text_with_graph_facts(
            text,
            {"graph_facts": [{"fact_id": "fact-1", "fact_text": "收养机构"}]},
            max_facts=3,
        )
        == text
    )


def test_graph_fact_budget_and_dedup_are_shared_across_memories():
    renderer = GraphFactContextRenderer(max_facts=2)

    first = renderer.enrich(
        "First memory.",
        {
            "graph_facts": [
                {"fact_id": "fact-1", "fact_text": "Alex likes jazz."},
                {"fact_id": "fact-2", "fact_text": "Alex visited Kyoto."},
            ]
        },
    )
    second = renderer.enrich(
        "Second memory.",
        {
            "graph_facts": [
                {"fact_id": "fact-1", "fact_text": "Alex likes jazz."},
                {"fact_id": "fact-3", "fact_text": "Alex works at Acme."},
            ]
        },
    )

    assert first.count("\n- ") == 2
    assert second == "Second memory."
