#!/usr/bin/env python3
"""DeepAgents x RAM-A prefetch-gain driver.

Runs a real DeepAgents agent whose model calls go to an OpenAI-compatible
endpoint (normally the UCM vllm-router), while ``RamAKvMiddleware`` reports
``turn_start`` / ``turn_end`` to a ram-a-kv daemon. The quantity of interest is
the per-turn latency difference between two modes:

  * ``cold``     the near tier (DDR / FA buffer) does NOT hold the session's KV
                 and the daemon does NOT prefetch, so the engine re-reads the KV
                 from SSD during prefill — the baseline. The near tier is small
                 and shared, so other requests routinely evict a session's blocks
                 to SSD; this is the state a later turn normally sees;
  * ``prefetch`` the near tier is cold, then the daemon prefetches the session's
                 blocks on turn_start, so the engine finds them resident.

The daemon's prefetch mode (and whether turn_start prefetch is sync or async) is
chosen by the caller (run_deepagent_gain.sh); this driver only talks to whatever
daemon URL it is handed.

Phases
------
``warm``     run the starting prompt once, persist the conversation as reusable
             context, and report how many KV chunk hashes the daemon recorded.
``measure``  reload the saved context, optionally flush the near tier cold,
             re-invoke the agent on it with a fixed probe question, and append
             one latency record to the results JSONL.
``report``   aggregate the results JSONL into a per-size, per-mode comparison
             (median / mean / stdev / min / max) and the speedup.

Two latency metrics are recorded for every measurement:

``latency_s``        the whole ``agent.invoke`` (graph dispatch + middleware +
                     model call) — what a user actually perceives as a turn.
``model_latency_s``  the raw chat-model call only (measured with a LangChain
                     callback) — the stage prefetch actually accelerates.
"""
from __future__ import annotations

import argparse
import json
import statistics
import sys
import time
import urllib.request
from pathlib import Path


# --------------------------------------------------------------------------- #
# HTTP helpers (stdlib only so the driver never depends on the SDK's httpx)
# --------------------------------------------------------------------------- #
def _post_json(url: str, obj: dict, headers: dict | None = None, timeout: float = 600):
    req = urllib.request.Request(
        url, data=json.dumps(obj).encode(),
        headers={"Content-Type": "application/json", **(headers or {})})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.status, json.loads(r.read().decode())


def _get_json(url: str, timeout: float = 10):
    with urllib.request.urlopen(url, timeout=timeout) as r:
        return json.loads(r.read().decode())


def resolve_model_name(model_base_url: str) -> str:
    return _get_json(model_base_url.rstrip("/") + "/models")["data"][0]["id"]


def session_map_chunk_hashes(daemon_url: str, session_id: str):
    """Ask the daemon for the KV map it recorded for a session (None if missing)."""
    _, resp = _post_json(
        daemon_url.rstrip("/") + "/event",
        {"type": "session_map", "session_id": session_id})
    if not resp.get("ok"):
        return None
    return resp.get("data", {}).get("chunk_hashes")


# --------------------------------------------------------------------------- #
# Cold flush: replay deterministic pool prompts through the router so the near
# tier (DDR / FA buffer) is evicted and the target session's blocks are cold.
# Reuses the UCM real-test pool layout (see 归档/real-test).
# --------------------------------------------------------------------------- #
def build_pool_prompt(tokens: int, nonce: str) -> str:
    sent = "The quick brown fox jumps over the lazy dog. "
    need = max(int(tokens * 4.4), 64)
    return f"nonce-{nonce}. " + (sent * (need // len(sent) + 2))[:need]


def flush_near_tier(pool_path: str, router_base: str, model_name: str, entries: int) -> int:
    pool = json.loads(Path(pool_path).read_text())
    replayed = 0
    for entry in pool["entries"][:entries]:
        prompt = build_pool_prompt(entry["prompt_tokens"], entry["nonce"])
        try:
            _post_json(
                router_base.rstrip("/") + "/v1/chat/completions",
                {"model": model_name,
                 "messages": [{"role": "user", "content": prompt}],
                 "max_tokens": 1, "temperature": 0},
                {"x-session-id": "deepagent-flush-" + entry["nonce"]})
            replayed += 1
        except Exception as exc:  # a failed flush entry only weakens the baseline
            print(f"[flush] {entry['nonce']} failed: {exc}", file=sys.stderr)
    return replayed


# --------------------------------------------------------------------------- #
# Agent construction (imported lazily so --help works without the venv)
# --------------------------------------------------------------------------- #
def make_agent(model_base_url: str, daemon_url: str, model_name: str, max_tokens: int,
               human_token_limit: int = 0):
    from deepagents import create_deep_agent
    from langchain_openai import ChatOpenAI
    from ram_a_middleware.sdk import RamAClient
    from ram_a_middleware.deepagents import RamAKvMiddleware

    model = ChatOpenAI(
        model=model_name,
        base_url=model_base_url,
        api_key="local-test",
        use_responses_api=False,   # KV transfer needs non-streaming Chat Completions
        streaming=False,
        temperature=0,
        max_tokens=max_tokens,
        timeout=600,
    )
    client = RamAClient(daemon_url, timeout=10)
    middleware = [RamAKvMiddleware(client)]
    if human_token_limit > 0:
        # Replace the default FilesystemMiddleware (deepagents merges custom
        # middleware by name) so a single long prompt is not evicted to a file.
        from deepagents.middleware.filesystem import FilesystemMiddleware
        middleware.append(
            FilesystemMiddleware(human_message_token_limit_before_evict=human_token_limit))
    agent = create_deep_agent(model=model, middleware=middleware)
    return agent, model


def _last_content(messages) -> str:
    try:
        content = messages[-1].content
        return content if isinstance(content, str) else json.dumps(content, ensure_ascii=False)
    except Exception:
        return ""


class ModelCallTimer:
    """LangChain callback that accumulates the raw chat-model call latency.

    This isolates the model call (where prefetch actually helps) from the rest
    of the agent turn (graph dispatch, middleware POSTs, message bookkeeping).

    It also records, for each model call, the wall-clock (epoch seconds) start
    and end plus the engine-issued request id, so the client-side stage can be
    aligned with the daemon (``turn_start`` / prefetch fan-out) and the engine
    (``FAWA request_id``) timestamps in a single end-to-end timeline.
    """

    def __init__(self):
        from langchain_core.callbacks import BaseCallbackHandler

        outer = self

        class _Handler(BaseCallbackHandler):
            def on_chat_model_start(self, serialized, messages, *, run_id, **kwargs):
                outer._starts[run_id] = time.perf_counter()
                outer.start_epochs.append(time.time())

            def on_llm_end(self, response, *, run_id, **kwargs):
                started = outer._starts.pop(run_id, None)
                if started is not None:
                    outer.total += time.perf_counter() - started
                    outer.calls += 1
                outer.end_epochs.append(time.time())
                # Capture the engine-reported token usage (authoritative x-axis)
                # and the engine-issued request id (aligns with the FAWA log line).
                try:
                    msg = response.generations[0][0].message
                    usage = getattr(msg, "usage_metadata", None) or {}
                    outer.prompt_tokens += int(usage.get("input_tokens") or 0)
                    outer.completion_tokens += int(usage.get("output_tokens") or 0)
                    rid = (getattr(msg, "response_metadata", None) or {}).get("id") \
                        or getattr(msg, "id", None)
                    if rid:
                        outer.request_ids.append(str(rid))
                except Exception:
                    pass

        self._starts: dict = {}
        self.total = 0.0
        self.calls = 0
        self.prompt_tokens = 0
        self.completion_tokens = 0
        self.start_epochs: list = []
        self.end_epochs: list = []
        self.request_ids: list = []
        self.handler = _Handler()


# --------------------------------------------------------------------------- #
# Phases
# --------------------------------------------------------------------------- #
def phase_warm(args) -> int:
    from langchain_core.messages import messages_to_dict

    prompt = Path(args.prompt_file).read_text(encoding="utf-8")
    model_name = resolve_model_name(args.model_base_url)
    agent, model = make_agent(args.model_base_url, args.daemon_url, model_name, args.max_tokens,
                              args.human_token_limit)
    try:
        config = {"configurable": {"thread_id": args.thread_id}}
        result = agent.invoke(
            {"messages": [{"role": "user", "content": prompt}]}, config=config)
        messages = result["messages"]
        Path(args.context).parent.mkdir(parents=True, exist_ok=True)
        Path(args.context).write_text(
            json.dumps(messages_to_dict(messages), ensure_ascii=False), encoding="utf-8")
    finally:
        model.root_client.close()

    session_id = "deepagents:" + args.thread_id
    hashes = session_map_chunk_hashes(args.daemon_url, session_id)
    print(json.dumps({
        "phase": "warm",
        "thread_id": args.thread_id,
        "session_id": session_id,
        "size_tokens": args.size,
        "messages": len(messages),
        "chunk_hashes": len(hashes) if hashes else 0,
        "assistant": _last_content(messages)[:200],
        "context_file": args.context,
    }, ensure_ascii=False))
    return 0 if hashes else 1


def phase_measure(args) -> int:
    from langchain_core.messages import HumanMessage, messages_from_dict

    model_name = resolve_model_name(args.model_base_url)
    if args.flush:
        if args.pool and Path(args.pool).exists():
            replayed = flush_near_tier(
                args.pool, args.router_base, model_name, args.flush_entries)
            print(f"[flush] replayed {replayed} pool entries through {args.router_base}",
                  file=sys.stderr)
        else:
            print(f"[flush] pool not found at {args.pool!r}; measuring without a cold flush",
                  file=sys.stderr)

    context = messages_from_dict(json.loads(Path(args.context).read_text()))
    messages = list(context) + [HumanMessage(content=args.probe)]

    agent, model = make_agent(args.model_base_url, args.daemon_url, model_name, args.max_tokens,
                              args.human_token_limit)
    timer = ModelCallTimer()
    try:
        config = {"configurable": {"thread_id": args.thread_id}, "callbacks": [timer.handler]}
        measure_start_epoch = time.time()
        started = time.perf_counter()
        result = agent.invoke({"messages": messages}, config=config)
        latency = time.perf_counter() - started
        measure_end_epoch = time.time()
        reply = _last_content(result["messages"])
    finally:
        model.root_client.close()

    record = {
        "phase": "measure",
        "mode": args.mode,               # "cold" | "prefetch"
        "flushed": bool(args.flush),     # was the near tier flushed cold before this run
        "thread_id": args.thread_id,
        "size_tokens": args.size,
        "rep": args.rep,
        "latency_s": round(latency, 4),
        "model_latency_s": round(timer.total, 4),
        "model_calls": timer.calls,
        "prompt_tokens": timer.prompt_tokens,
        "completion_tokens": timer.completion_tokens,
        "context_messages": len(context),
        "reply": reply[:120],
        # Wall-clock anchors (epoch seconds) for the end-to-end stage timeline:
        #   measure_*_epoch       : agent.invoke start/end (whole turn)
        #   model_*_epoch         : earliest chat-model start / latest chat-model end
        #   engine_request_ids    : engine chatcmpl ids (join key with the FAWA log)
        "measure_start_epoch": round(measure_start_epoch, 6),
        "measure_end_epoch": round(measure_end_epoch, 6),
        "model_start_epoch": round(min(timer.start_epochs), 6) if timer.start_epochs else None,
        "model_end_epoch": round(max(timer.end_epochs), 6) if timer.end_epochs else None,
        "engine_request_ids": timer.request_ids,
        "ts": time.time(),
    }
    Path(args.results).parent.mkdir(parents=True, exist_ok=True)
    with open(args.results, "a", encoding="utf-8") as fh:
        fh.write(json.dumps(record, ensure_ascii=False) + "\n")
    print(json.dumps(record, ensure_ascii=False))
    return 0


def _stats(values: list[float]) -> dict:
    if not values:
        return {}
    return {
        "n": len(values),
        "median": round(statistics.median(values), 4),
        "mean": round(statistics.fmean(values), 4),
        "stdev": round(statistics.stdev(values), 4) if len(values) > 1 else 0.0,
        "min": round(min(values), 4),
        "max": round(max(values), 4),
    }


def phase_report(args) -> int:
    records = [json.loads(line) for line in Path(args.results).read_text().splitlines() if line.strip()]
    if args.min_size > 0:
        records = [r for r in records if r.get("size_tokens", 0) >= args.min_size]
    if not records:
        print("no measurement records found", file=sys.stderr)
        return 1

    metrics = ["latency_s", "model_latency_s"]
    info_metrics = ["prompt_tokens"]
    # "cold" is the canonical baseline; keep "no-prefetch" as a legacy alias.
    baseline = "cold" if any(r.get("mode") == "cold" for r in records) else "no-prefetch"
    sizes = sorted({r.get("size_tokens", 0) for r in records})
    env: dict = {}
    if args.env_file and Path(args.env_file).exists():
        try:
            env = json.loads(Path(args.env_file).read_text())
        except Exception as exc:  # a malformed snapshot must not sink the report
            print(f"[report] ignoring malformed env file: {exc}", file=sys.stderr)

    summary: dict = {"env": env, "baseline": baseline, "records": len(records), "sizes": {}}
    for size in sizes:
        per_mode: dict[str, dict[str, list[float]]] = {}
        for rec in records:
            if rec.get("size_tokens", 0) != size:
                continue
            bucket = per_mode.setdefault(rec["mode"], {m: [] for m in metrics + info_metrics})
            for m in metrics + info_metrics:
                if m in rec:
                    bucket[m].append(rec[m])

        entry: dict = {
            "modes": {mode: {m: _stats(v) for m, v in d.items()} for mode, d in per_mode.items()},
            "comparisons": {},
        }
        base_stats = entry["modes"].get(baseline, {})
        if "prefetch" in entry["modes"]:
            comp: dict = {}
            for m in metrics:
                base_med = base_stats.get(m, {}).get("median")
                test_med = entry["modes"]["prefetch"].get(m, {}).get("median")
                if base_med and test_med:
                    comp[f"speedup_{m}"] = round(base_med / test_med, 3)
                    comp[f"saved_ms_{m}"] = round((base_med - test_med) * 1000, 1)
            entry["comparisons"]["prefetch_vs_baseline"] = comp
        summary["sizes"][str(size)] = entry

    Path(args.summary).write_text(json.dumps(summary, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps(summary, ensure_ascii=False, indent=2))
    md = _report_markdown(summary)
    print("\n" + md)
    if args.report_md:
        Path(args.report_md).parent.mkdir(parents=True, exist_ok=True)
        Path(args.report_md).write_text(md + "\n", encoding="utf-8")
    if args.result_md:
        rmd = _result_markdown(summary, time.strftime("%Y-%m-%d %H:%M:%S"))
        Path(args.result_md).parent.mkdir(parents=True, exist_ok=True)
        Path(args.result_md).write_text(rmd, encoding="utf-8")
        print(f"[report] at-a-glance result -> {args.result_md}", file=sys.stderr)
    return 0


def _md_table(header: list[str], rows: list[list[str]]) -> str:
    lines = ["| " + " | ".join(header) + " |",
             "|" + "|".join(["---"] * len(header)) + "|"]
    lines += ["| " + " | ".join(str(c) for c in row) + " |" for row in rows]
    return "\n".join(lines)


def _report_markdown(summary: dict) -> str:
    base = summary["baseline"]
    out = ["# DeepAgents x RAM-A prefetch-gain report", ""]
    env = summary.get("env") or {}
    if env:
        out += ["## Run environment", "",
                _md_table(["key", "value"], [[k, v] for k, v in env.items()]), ""]
    out += [
        f"Baseline mode: **{base}** (near tier cold, no prefetch). "
        f"`prefetch` = cold, then the daemon prefetches on turn_start. "
        f"Records: {summary['records']}.",
        "",
        "## Per-mode latency",
        "",
    ]
    rows = []
    for size, entry in summary["sizes"].items():
        for mode in ("cold", "no-prefetch", "prefetch"):
            st = entry["modes"].get(mode)
            if not st:
                continue
            turn, model = st.get("latency_s", {}), st.get("model_latency_s", {})
            rows.append([size, mode, turn.get("n", "-"),
                         turn.get("median", "-"), turn.get("stdev", "-"),
                         model.get("median", "-"), model.get("stdev", "-")])
    out += [_md_table(["context(tok)", "mode", "n", "turn med(s)", "turn sd",
                       "model med(s)", "model sd"], rows), "",
            "## Comparison vs baseline", ""]
    rows = []
    for size, entry in summary["sizes"].items():
        comp = entry["comparisons"].get("prefetch_vs_baseline")
        if not comp:
            continue
        rows.append([
            size,
            comp.get("speedup_latency_s", "-"),
            comp.get("speedup_model_latency_s", "-"),
            comp.get("saved_ms_model_latency_s", "-"),
        ])
    out += [_md_table(["context(tok)", "turn speedup", "model speedup", "model saved(ms)"], rows)]
    return "\n".join(out)


def _mean(entry: dict, mode: str, metric: str):
    """Mean of one metric for one mode, or None when the mode/sample is absent."""
    return entry["modes"].get(mode, {}).get(metric, {}).get("mean")


def _result_markdown(summary: dict, ts: str) -> str:
    """At-a-glance result: per-size mean latency and improvement %.

    Kept deliberately short so a reader can open this one file and immediately
    see 'average latency' and 'how much prefetch improved it' at every context
    size, without parsing the full report.
    """
    base = summary["baseline"]
    env = summary.get("env") or {}
    out = [
        "# DeepAgents × RAM-A 预取收益 · 结果速览",
        "",
        f"_生成时间:{ts}_",
        "",
        "| 档位 | 近端(FA/DDR) | daemon 预取 | 含义 |",
        "| --- | --- | --- | --- |",
        "| `cold` | 冷(被其他请求驱逐) | 关 | **基线** |",
        "| `prefetch` | 冷(被其他请求驱逐) | **开(异步)** | 待测收益 |",
        "",
        f"- 数据:{summary['records']} 条原始测量;明细见同目录 `results.jsonl` / `summary.json` / `report.md`",
    ]
    if env:
        out.append(
            f"- 环境:git `{env.get('git_commit', '?')}` · router `{env.get('ucm_router', '?')}` · "
            f"flush {env.get('flush_entries', '?')} 条 · reps {env.get('reps', '?')}")
    out += ["", "## 整轮平均时延(agent 一次交互)", ""]
    rows, turn_imps = [], []
    for size, entry in summary["sizes"].items():
        cold = _mean(entry, base, "latency_s")
        pf = _mean(entry, "prefetch", "latency_s")
        imp = (cold - pf) / cold * 100 if (cold and pf) else None
        if imp is not None:
            turn_imps.append(imp)
        rows.append([
            size,
            f"{cold:.3f}" if cold else "-",
            f"{pf:.3f}" if pf else "-",
            f"{cold / pf:.3f}×" if (cold and pf) else "-",
            f"{imp:+.1f}%" if imp is not None else "-",
        ])
    out.append(_md_table(
        ["上下文(tok)", "冷态 cold(s)", "预取 prefetch(s)", "加速比", "提升"], rows))
    if turn_imps:
        out += ["", f"**整轮:在 {len(turn_imps)} 个尺寸上平均降低 "
                    f"{statistics.fmean(turn_imps):.1f}%(单尺寸 {min(turn_imps):+.1f}% ~ {max(turn_imps):+.1f}%)。**"]
    out += ["", "## 模型调用平均时延(预填充 + 解码)", ""]
    rows, model_imps = [], []
    for size, entry in summary["sizes"].items():
        cold = _mean(entry, base, "model_latency_s")
        pf = _mean(entry, "prefetch", "model_latency_s")
        imp = (cold - pf) / cold * 100 if (cold and pf) else None
        if imp is not None:
            model_imps.append(imp)
        rows.append([
            size,
            f"{cold:.3f}" if cold else "-",
            f"{pf:.3f}" if pf else "-",
            f"{(cold - pf) * 1000:.0f}" if (cold and pf) else "-",
            f"{imp:+.1f}%" if imp is not None else "-",
        ])
    out.append(_md_table(
        ["上下文(tok)", "冷态 cold(s)", "预取 prefetch(s)", "省时(ms)", "提升"], rows))
    if model_imps:
        out += ["", f"**模型调用:在 {len(model_imps)} 个尺寸上平均降低 {statistics.fmean(model_imps):.1f}%。**"]
    out += ["", "> 提升 =(冷态 − 预取)/ 冷态;加速比 = 冷态 / 预取;均为各尺寸内的样本均值(mean)。"]
    return "\n".join(out) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description="DeepAgents x RAM-A prefetch-gain driver")
    ap.add_argument("--phase", required=True, choices=["warm", "measure", "report"])
    ap.add_argument("--prompt-file", default="", help="warm phase: file holding the starting user prompt")
    ap.add_argument("--probe", default="请用一句话复述上文的核心要点。",
                    help="measure phase: probe appended to the saved context")
    ap.add_argument("--thread-id", default="deepagent-gain")
    ap.add_argument("--mode", default="prefetch",
                    choices=["cold", "prefetch", "no-prefetch"],
                    help="measure phase: 'cold' (near tier cold, no prefetch) | "
                         "'prefetch' (cold, then daemon prefetches)")
    ap.add_argument("--size", type=int, default=0, help="context size in tokens (label for the report)")
    ap.add_argument("--rep", type=int, default=0, help="repetition index (label)")
    ap.add_argument("--model-base-url", default="http://127.0.0.1:9003/v1")
    ap.add_argument("--daemon-url", default="http://127.0.0.1:7098")
    ap.add_argument("--router-base", default="http://127.0.0.1:9003")
    ap.add_argument("--context", default="data/context.json")
    ap.add_argument("--results", default="data/results.jsonl")
    ap.add_argument("--summary", default="data/summary.json")
    ap.add_argument("--report-md", default="data/report.md", help="report phase: markdown table output")
    ap.add_argument("--result-md", default="data/RESULT.md",
                    help="report phase: at-a-glance result (mean latency + improvement %%) output")
    ap.add_argument("--env-file", default="", help="report phase: JSON snapshot of the run environment")
    ap.add_argument("--min-size", type=int, default=0,
                    help="report phase: drop records whose size_tokens < this (e.g. 32768)")
    ap.add_argument("--max-tokens", type=int, default=32)
    ap.add_argument("--human-token-limit", type=int, default=0,
                    help="if >0, raise FilesystemMiddleware.human_message_token_limit_before_evict "
                         "so a single long prompt is not evicted to a file")
    ap.add_argument("--flush", action="store_true", help="measure phase: flush near tier before running")
    ap.add_argument("--pool", default="")
    ap.add_argument("--flush-entries", type=int, default=12)
    args = ap.parse_args()

    if args.phase == "warm":
        if not args.prompt_file:
            print("--prompt-file is required for the warm phase", file=sys.stderr)
            return 2
        return phase_warm(args)
    if args.phase == "measure":
        return phase_measure(args)
    return phase_report(args)


if __name__ == "__main__":
    sys.exit(main())
