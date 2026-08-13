import sys
import os

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from common.metrics import recall_at_k, mrr, ndcg_at_k


def test_recall_at_k():
    # Hit: all relevant items in top k
    assert recall_at_k(["a", "b", "c"], ["a", "b"], k=3) == 1.0

    # Partial: only some relevant items in top k
    result = recall_at_k(["a", "c", "d", "b"], ["a", "b"], k=2)
    assert result == 0.5

    # Miss: no relevant items in top k
    assert recall_at_k(["x", "y", "z"], ["a", "b"], k=3) == 0.0

    # k=3 vs k=5: same list, different k
    retrieved = ["a", "b", "c", "d", "e"]
    relevant = ["c", "e"]
    assert recall_at_k(retrieved, relevant, k=3) == 0.5
    assert recall_at_k(retrieved, relevant, k=5) == 1.0

    # Empty relevant list returns 0.0
    assert recall_at_k(["a", "b"], [], k=5) == 0.0

    # Empty retrieved list
    assert recall_at_k([], ["a"], k=5) == 0.0


def test_mrr():
    # First position
    assert mrr(["a", "b", "c"], ["a"]) == 1.0

    # Third position
    assert mrr(["x", "y", "a"], ["a"]) == 1.0 / 3.0

    # No hit
    assert mrr(["x", "y", "z"], ["a"]) == 0.0

    # Multiple relevant, first at position 2
    assert mrr(["x", "a", "b"], ["a", "b"]) == 0.5

    # Empty lists
    assert mrr([], ["a"]) == 0.0
    assert mrr(["a"], []) == 0.0


def test_ndcg_at_k():
    # Perfect: all relevant at top positions
    result = ndcg_at_k(["a", "b", "c"], ["a", "b", "c"], k=3)
    assert result == 1.0

    # One relevant at position 2
    result = ndcg_at_k(["x", "a"], ["a"], k=5)
    expected = (1.0 / (2 ** 0.5)) / (1.0 / (2 ** 1.0))  # dcg: 1/log2(3), idcg: 1/log2(2)
    import math
    expected = (1.0 / math.log2(3)) / (1.0 / math.log2(2))
    assert abs(result - expected) < 1e-9

    # No hit: no relevant items retrieved
    assert ndcg_at_k(["x", "y", "z"], ["a", "b"], k=3) == 0.0

    # Empty relevant returns 0.0
    assert ndcg_at_k(["a", "b"], [], k=5) == 0.0

    # Empty retrieved
    assert ndcg_at_k([], ["a"], k=5) == 0.0

    # k=1 with hit
    assert ndcg_at_k(["a", "b"], ["a"], k=1) == 1.0

    # k=1 with miss
    assert ndcg_at_k(["x", "a"], ["a"], k=1) == 0.0

    # Multiple relevant, partial retrieval
    result = ndcg_at_k(["a", "x", "b"], ["a", "b", "c"], k=5)
    assert 0.0 < result < 1.0


if __name__ == "__main__":
    test_recall_at_k()
    test_mrr()
    test_ndcg_at_k()
    print("all metrics tests passed")
