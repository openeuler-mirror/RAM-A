#!/usr/bin/env python3
"""Generate a deterministic, high-entropy prompt of ~N tokens for the gain sweep.

The same ``(tokens, seed)`` always yields byte-identical text, so the
no-prefetch and prefetch runs of a given size share an identical context.
Different sizes default to different seeds so one size's text is not a prefix
of another's, avoiding cross-size prefix-cache interference.

The vocabulary is large and the word order is random, so every 128-token KV
block is (almost surely) distinct. This matters: a repetitive prompt collapses
into very few distinct blocks and produces almost no KV to prefetch.
"""
from __future__ import annotations

import argparse
import json
import random
from pathlib import Path

WORDS = (
    "adapter allocator arena asynchronously backpressure bandwidth barrier batch "
    "buffer cache capacity checker checksum chunk cluster coalesce coherence commit "
    "compaction compression concurrency connector consumer contention controller copy "
    "coroutine decode degradation delimiter descriptor dirty dispatch dma eviction "
    "fabric fallback fanout flush fragment gateway handle hash hit hotpath indexing "
    "inference ingress invalidate kernel latency layer lease locality mapping memory "
    "migration miss namespace node numa offset overflow page partition payload pipeline "
    "placement prefetch prefill priority promotion proxy quantum queue rank reclaim "
    "refcount region replica residency restart resume router runtime scheduler segment "
    "semaphore shard slab snapshot spill staging stripe subgraph swap telemetry tenant "
    "throttle tier timeout token topology trace transfer ttl upstream utilization vector "
    "version warmup watermark worker workload zero".split()
)


def build(tokens: int, seed: int) -> str:
    rnd = random.Random(seed)
    target = int(tokens * 4.4)
    header = "请阅读并总结下面这段技术材料,然后用三句话说明它的要点。"
    footer = "请用三句话总结以上材料。"
    lines = [header, ""]
    used = len(header) + len(footer) + 4
    idx = 0
    while used < target:
        seq = " ".join(rnd.choice(WORDS) for _ in range(24))
        line = f"[{idx:05d}] {seq}"
        lines.append(line)
        used += len(line) + 1
        idx += 1
    lines += ["", footer]
    return "\n".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser(description="deterministic prompt generator for the sweep")
    ap.add_argument("--tokens", type=int, required=True, help="target context size in tokens")
    ap.add_argument("--seed", type=int, default=None, help="RNG seed (default: tokens)")
    ap.add_argument("--out", required=True, help="output file path")
    args = ap.parse_args()

    seed = args.seed if args.seed is not None else args.tokens
    text = build(args.tokens, seed)
    Path(args.out).write_text(text, encoding="utf-8")
    print(json.dumps({"out": args.out, "chars": len(text),
                      "est_tokens": round(len(text) / 4.4), "seed": seed}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
