#!/usr/bin/env python3
# Copyright (c) 2026 Huawei Technologies Co.,Ltd. All rights reserved.
#
# RAM-A is licensed under Mulan PSL v2.
# You can use this software according to the terms and conditions of the Mulan
# PSL v2.
# You may obtain a copy of Mulan PSL v2 at:
#         http://license.coscl.org.cn/MulanPSL2
# THIS SOFTWARE IS PROVIDED ON AN "AS IS" BASIS, WITHOUT WARRANTIES OF ANY
# KIND, EITHER EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO
# NON-INFRINGEMENT, MERCHANTABILITY OR FIT FOR A PARTICULAR PURPOSE.
# See the Mulan PSL v2 for more details.

"""Generate a deterministic, high-entropy prompt of ~N *real* tokens.

The same ``(tokens, seed)`` always yields byte-identical text, so the
no-prefetch and prefetch runs of a given size share an identical context.
Different sizes default to different seeds so one size's text is not a prefix
of another's, avoiding cross-size prefix-cache interference.

The vocabulary is large and the word order is random, so every KV block is
(almost surely) distinct. This matters: a repetitive prompt collapses into very
few distinct blocks and produces almost no KV to prefetch.

Token count is calibrated against the serving engine's *real* tokenizer (the
vLLM ``/tokenize`` endpoint) so the text really is ~N tokens. A fixed
chars/token guess is wrong here: this high-entropy vocabulary tokenizes at
~5.3-5.9 chars/token, so the old 4.4 guess undershot by ~20%. When no tokenizer
endpoint is reachable the generator falls back to that observed constant.

``--tokens`` is the *whole-prompt* target: the engine's reported
``prompt_tokens`` also counts the fixed agent scaffold (DeepAgents system
prompt, the warm reply, the probe question and the chat template). That
scaffold, ``--overhead-tokens``, is subtracted from the material so the engine
sees ~``--tokens`` in total; leave it 0 to size the material itself instead.
"""
from __future__ import annotations

import argparse
import json
import os
import random
import sys
import urllib.request
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

# Observed chars/token for this vocabulary; used only when the engine's
# tokenizer is unreachable (primary path calibrates against the real one).
FALLBACK_CPT = 5.6
INITIAL_CPT = 5.6


def build_chars(nchars: int, seed: int) -> str:
    """Deterministic text of roughly ``nchars`` characters (line-granular)."""
    rnd = random.Random(seed)
    header = "请阅读并总结下面这段技术材料,然后用三句话说明它的要点。"
    footer = "请用三句话总结以上材料。"
    lines = [header, ""]
    used = len(header) + len(footer) + 4
    idx = 0
    while used < nchars:
        seq = " ".join(rnd.choice(WORDS) for _ in range(24))
        line = f"[{idx:05d}] {seq}"
        lines.append(line)
        used += len(line) + 1
        idx += 1
    lines += ["", footer]
    return "\n".join(lines)


def _post_json(url: str, obj: dict, timeout: float):
    req = urllib.request.Request(
        url, data=json.dumps(obj).encode(),
        headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read().decode())


def make_tokenizer(base_url: str, model: str, timeout: float = 300.0):
    """Return a ``text -> token count`` callable backed by the engine tokenizer.

    ``None`` when ``base_url`` is empty or the model name cannot be resolved;
    per-call failures are surfaced by the caller so it can fall back.
    """
    if not base_url:
        return None
    base = base_url.rstrip("/")
    if not model:
        try:
            with urllib.request.urlopen(base + "/v1/models", timeout=30) as r:
                model = json.loads(r.read().decode())["data"][0]["id"]
        except Exception as exc:
            print(f"[gen_prompt] cannot resolve model from {base}/v1/models: {exc}",
                  file=sys.stderr)
            return None
    url = base + "/tokenize"

    def count(text: str) -> int:
        return int(_post_json(url, {"model": model, "prompt": text}, timeout)["count"])

    return count


def build(tokens: int, seed: int, count=None, tol_frac: float = 0.003, max_iters: int = 8):
    """Return ``(text, actual_tokens_or_None)``.

    With ``count`` (a real tokenizer callable) iterate the character budget so
    the tokenized length lands within ``tol_frac`` of ``tokens``; without one,
    fall back to the observed chars/token constant.
    """
    if count is None:
        return build_chars(int(tokens * FALLBACK_CPT), seed), None

    tol = max(8, int(tokens * tol_frac))
    cpt = INITIAL_CPT
    text = build_chars(int(tokens * cpt), seed)
    actual = None
    for _ in range(max_iters):
        text = build_chars(int(tokens * cpt), seed)
        try:
            actual = count(text)
        except Exception as exc:
            print(f"[gen_prompt] tokenize failed ({exc}); keeping last guess", file=sys.stderr)
            return text, None
        if abs(actual - tokens) <= tol:
            break
        cpt *= tokens / actual
    return text, actual


def main() -> int:
    ap = argparse.ArgumentParser(description="deterministic prompt generator for the sweep")
    ap.add_argument("--tokens", type=int, default=0,
                    help="target whole-prompt size in tokens (engine prompt_tokens)")
    ap.add_argument("--overhead-tokens", type=int,
                    default=int(os.environ.get("SCAFFOLD_TOKENS", "0")),
                    help="fixed agent scaffold tokens to subtract from --tokens "
                         "(system prompt + warm reply + probe + template)")
    ap.add_argument("--seed", type=int, default=None, help="RNG seed (default: tokens)")
    ap.add_argument("--out", default="", help="output file path")
    ap.add_argument("--count-file", default="",
                    help="print the real token count of FILE (via the tokenizer) and exit")
    ap.add_argument("--tokenize-url", default=os.environ.get("TOKENIZE_URL", ""),
                    help="engine/router base exposing /tokenize (empty = no calibration)")
    ap.add_argument("--model", default=os.environ.get("TOKENIZE_MODEL", ""),
                    help="model id for /tokenize (default: resolved from /v1/models)")
    ap.add_argument("--tolerance", type=float, default=0.003,
                    help="relative token tolerance for calibration")
    ap.add_argument("--max-iters", type=int, default=8)
    args = ap.parse_args()

    count = make_tokenizer(args.tokenize_url, args.model)

    if args.count_file:
        if count is None:
            print(f"[gen_prompt] tokenizer unavailable; cannot count {args.count_file}",
                  file=sys.stderr)
            return 1
        print(count(Path(args.count_file).read_text(encoding="utf-8")))
        return 0

    if not args.tokens or not args.out:
        ap.error("--tokens and --out are required (or use --count-file)")

    seed = args.seed if args.seed is not None else args.tokens
    material = max(1, args.tokens - max(0, args.overhead_tokens))
    text, actual = build(material, seed, count, args.tolerance, args.max_iters)
    Path(args.out).write_text(text, encoding="utf-8")
    print(json.dumps({
        "out": args.out,
        "chars": len(text),
        "target_tokens": args.tokens,
        "overhead_tokens": args.overhead_tokens,
        "material_target_tokens": material,
        "actual_tokens": actual,
        "cpt": round(len(text) / actual, 3) if actual else round(len(text) / material, 3),
        "calibrated": actual is not None,
        "seed": seed,
    }))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
