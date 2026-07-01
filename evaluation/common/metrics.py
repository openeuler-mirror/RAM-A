import math


def recall_at_k(retrieved_ids: list[str], relevant_ids: list[str], k: int) -> float:
    if not relevant_ids:
        return 0.0
    top_k = set(retrieved_ids[:k])
    hits = sum(1 for rid in relevant_ids if rid in top_k)
    return hits / len(relevant_ids)


def mrr(retrieved_ids: list[str], relevant_ids: list[str]) -> float:
    relevant_set = set(relevant_ids)
    for rank, rid in enumerate(retrieved_ids, start=1):
        if rid in relevant_set:
            return 1.0 / rank
    return 0.0


def ndcg_at_k(retrieved_ids: list[str], relevant_ids: list[str], k: int) -> float:
    relevant_set = set(relevant_ids)
    if not relevant_set:
        return 0.0
    dcg = 0.0
    for rank, rid in enumerate(retrieved_ids[:k], start=1):
        if rid in relevant_set:
            dcg += 1.0 / math.log2(rank + 1)
    idcg = sum(1.0 / math.log2(rank + 1) for rank in range(1, min(len(relevant_ids), k) + 1))
    if idcg == 0.0:
        return 0.0
    return dcg / idcg
