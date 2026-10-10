#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""UCM 预取效果对照测试(快速版):flush 改为"重放预放池",不写盘,整轮 ~5 分钟。
"""
from __future__ import annotations
import argparse
import glob
import json
import math
import os
import random
import statistics
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from concurrent.futures import FIRST_COMPLETED, ThreadPoolExecutor, wait

BLOCK_BYTES_DEFAULT = 798720
LOG_DIR_DEFAULT = "/mnt/sdb/ucm/logs"
POOL_FILE_DEFAULT = "/mnt/sdb/ucm/flush_pool.json"
OUT_DIR_DEFAULT = (os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
                   + "/reports/data/t1")
# DDR 缓存总量(节点级,GiB)。随部署不同,用 --cache-buffer-capacity-gb 或
# 环境变量 UCM_CACHE_BUFFER_CAPACITY_GB 指定;都不给时用该默认值。
DDR_FALLBACK_GB = 64.0


def http_json(method, url, body=None, timeout=900.0, headers=None):
    data = None if body is None else json.dumps(body).encode("utf-8")
    hdrs = {"Content-Type": "application/json"}
    if headers:
        hdrs.update(headers)
    req = urllib.request.Request(url, data=data, headers=hdrs, method=method)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return resp.status, json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", "replace")
        try:
            return e.code, json.loads(raw)
        except ValueError:
            return e.code, {"ok": False, "error": raw}


def build_prompt(tokens: int, nonce: str) -> str:
    """nonce 打头保证前缀哈希唯一,后接重复句子凑长度(约 4.4 字符/token)。"""
    sent = "The quick brown fox jumps over the lazy dog. "
    head = f"nonce-{nonce}. "
    need = max(int(tokens * 4.4), 64)
    body = (sent * (need // len(sent) + 2))[:need]
    return head + body


# router 断路器瞬态:压力相位把引擎打满 ⇒ /health 探针(默认 5s)超时 ⇒ worker 被判 unhealthy,
# 断路器在 120s 窗口内累计 10 次失败后 closed -> open ⇒ router 对**所有**请求直接回 503
# "No available workers (all circuits open or unhealthy)"。
# 该场景下整轮仅发出少量请求即中断。退避总时长必须 > open(60s) + 健康恢复(~180s)。
RETRY_503_BACKOFF = (0.0, 2.0, 5.0, 10.0, 20.0, 40.0, 60.0, 60.0, 60.0)   # 累计 ≈257s


def chat(url, model, prompt, max_tokens, timeout, session_id=None) -> dict:
    """非流式 + max_tokens=1 时,wall-clock 近似等于 TTFT(KV 回源发生在 prefill 前)。

    只对 **503**(router 断路器/健康检查瞬态,与 UCM 无关)指数退避重试;
    其它非 200(404/400 等)是真实错误,立即抛出。elapsed 按**成功那一次**单独计时。
    """
    body = {
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": max_tokens,
        "temperature": 0,
    }
    headers = {"Content-Type": "application/json"}
    if session_id is not None:
        headers["x-session-id"] = session_id
    code, d, elapsed = None, None, None
    for attempt, wait in enumerate(RETRY_503_BACKOFF, start=1):
        if wait:
            time.sleep(wait)
        t0 = time.perf_counter()
        code, d = http_json("POST", f"{url}/v1/chat/completions", body, timeout, headers)
        elapsed = time.perf_counter() - t0
        if code == 503:
            print(f"    [warn] HTTP 503(router 断路器瞬态) 第 {attempt}/{len(RETRY_503_BACKOFF)} 次,"
                  f"退避 {wait:.0f}s 后重试", flush=True)
            continue
        break
    if code != 200:
        raise RuntimeError(f"vllm HTTP {code}: {d}")
    kt = d.get("kv_transfer_params") or {}
    return {
        "id": d.get("id"),
        "elapsed_s": elapsed,
        "prompt_tokens": d["usage"]["prompt_tokens"],
        "block_ids": kt.get("ucm_block_ids") or [],
        "rank": kt.get("prefetch_rank"),
    }


def prefetch_warm(base, block_ids, chunk, timeout, headers=None) -> dict:
    """按 chunk 切分调用 vllm-router /kv/prefetch。headers 传 x-session-id:
    router 与推理共享同一 policy 实例选路,预取落点==推理落点。"""
    total_ok = 0
    modes, dp_ranks = set(), set()
    t0 = time.perf_counter()
    for i in range(0, len(block_ids), chunk):
        payload = {"block_ids": block_ids[i : i + chunk]}
        code, d = http_json("POST", f"{base}/kv/prefetch", payload, timeout, headers=headers)
        if code != 200 or not d.get("ok"):
            raise RuntimeError(f"router 预取失败 HTTP {code}: {d}")
        total_ok += int(d.get("prefetch_count", 0))
        modes.add(d.get("mode"))
        dp_ranks.add(d.get("dp_rank"))
    return {
        "prefetch_count": total_ok,
        "modes": sorted(modes),
        "dp_ranks": sorted(dp_ranks),
        "elapsed_s": time.perf_counter() - t0,
    }


def drop_caches() -> bool:
    try:
        subprocess.run(["sync"], check=False)
        with open("/proc/sys/vm/drop_caches", "w") as f:
            f.write("3\n")
        return True
    except OSError as e:
        print(f"    [warn] drop_caches 失败: {e}")
        return False


def latest_vllm_log(log_dir):
    files = glob.glob(os.path.join(log_dir, "vllm-*.log"))
    return max(files, key=os.path.getmtime) if files else None


def scan_fawa(log_file, request_ids: set) -> dict:
    """从 worker 日志捞 FAWA 行,确认每个请求的 hit hbm / hit external / load blocks。

    **每个请求只取第一条 FAWA 行**。
    ---------------------------------------------------
    引擎开 `--max-num-batched-tokens`,长 prompt 走 **chunked prefill**:连接器的
    `get_num_new_matched_tokens` 每个 chunk 被调一次,每次都打一行 FAWA,而
    `num_computed_tokens`(=日志里的 `hit hbm tokens`)随 chunk 单调增长
    (0 → 8192 → … → prompt−最后一个 chunk)。256409-token 的压力请求产生 **31** 行,
    128257-token 的被测请求打了 **3** 行(0 / 102400 / 114688)。

    若用 `out[base] = item`(后写覆盖)会取到**最后一行**,即
    `hit hbm tokens ≈ 整条 prompt` —— 会使被测样本被判成
    `hbm_resident`,`hbm==0` 的样本恒判 FAIL。同一请求三行示例:
      hbm=0      ext=96128  load=751  dump=251   ← 第一条 = 真实「本地 HBM 命中」
      hbm=102400 ext=3840   load=30   dump=172
      hbm=114688 ext=0      load=0    dump=93    ← 最后一行

    所以改成「先写优先」(`setdefault`):第一条 `num_computed_tokens` 才是请求调度之初
    vLLM 本地前缀缓存给的命中量,也正是 V1/V2 判据想要的量。
    """
    out: dict = {}
    if not log_file or not os.path.exists(log_file):
        return out
    with open(log_file, "r", errors="replace") as f:
        for line in f:
            if "FAWA request_id" not in line:
                continue
            rid = line.split("FAWA request_id:", 1)[1].split(",", 1)[0].strip()
            base = rid.rsplit("-", 1)[0]
            if base not in request_ids or base in out:
                continue
            item = {"raw": line.strip()}
            for key, tag in (
                ("total", "total tokens: "),
                ("hbm", "hit hbm tokens: "),
                ("external", "hit external tokens: "),
                ("load", "load blocks: "),
                ("dump", "dump blocks: "),
            ):
                if tag in line:
                    try:
                        item[key] = int(line.split(tag, 1)[1].split(",", 1)[0])
                    except ValueError:
                        pass
            out[base] = item
    return out


def wait_fawa(log_file, request_id, tries, interval) -> dict:
    """日志异步落盘,轮询等待该请求的 FAWA 行出现。"""
    if not log_file:
        return {}
    for _ in range(max(1, tries)):
        hit = scan_fawa(log_file, {request_id})
        if hit:
            return hit.get(request_id, {})
        time.sleep(interval)
    return {}


def measure(args, prompt, expect_rank, label, log_file, sid=None):
    """跑一次并计时;FAWA hit hbm tokens==0 才算有效冷/热样本,否则换 sid 重试。"""
    for attempt in range(1, args.retries + 1):
        attempt_sid = f"{sid}#t{attempt}" if sid else None
        s = chat(args.vllm_url, args.model, prompt, args.max_tokens, args.timeout,
                 session_id=attempt_sid)
        f = wait_fawa(log_file, s["id"], args.log_wait_tries, args.log_wait_s)
        hbm = f.get("hbm")
        if hbm is None and not s["block_ids"] and s["rank"] is None:
            # 既无 FAWA 行、响应里也没有 UCM 句柄(ucm_block_ids/prefetch_rank)
            # = 该请求根本没经过 UCM 外部缓存:典型原因是整段 KV 已命中该 rank 的
            # HBM(vLLM 本地 prefix cache 先命中,connector 未被调用)。
            # 这种样本既无法确认介质来源,也不该计入冷/热耗时 -> 换 sid 重试
            # (sid 变了 -> consistent_hash 落点变了 -> 规避残留 HBM 的那个 rank)。
            valid = False
            note = "无UCM句柄-疑似HBM全中-作废"
        elif hbm is None:
            valid = s["rank"] != expect_rank
            note = "日志缺-按rank判定" if valid else "日志缺-疑似HBM-作废"
        else:
            valid = hbm == 0
            note = "ok" if valid else f"HBM命中{hbm}tok-作废"
        print(f"    {label:<4} #{attempt}: {s['elapsed_s']:6.2f}s  rank={s['rank']}"
              f"  blocks={len(s['block_ids'])}  load={f.get('load', '?')}  [{note}]",
              flush=True)
        if valid:
            s["fawa"] = f
            return s
    return None


# ===== 预放池 =====

def pool_load(path: str) -> dict:
    with open(path, "r", encoding="utf-8") as f:
        return json.load(f)


def pool_save(path: str, pool: dict) -> None:
    """原子写,避免中断留下半截池文件。"""
    d = os.path.dirname(path) or "."
    fd, tmp = tempfile.mkstemp(dir=d, prefix=".flush_pool.", suffix=".tmp")
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        json.dump(pool, f, ensure_ascii=False, indent=1)
    os.replace(tmp, path)


def env_float(name: str, dflt: float) -> float:
    """读浮点环境变量,缺失/非法则用默认值。"""
    try:
        return float(os.environ.get(name, "") or dflt)
    except ValueError:
        return dflt


def resolve_ddr(args) -> float:
    """确定 DDR 总量,换算 FA(预取缓存)侧容量(GiB),并打印来源与口径。

    `cache_buffer_capacity_gb` 是**节点级**总量,hma_connector 对半切给 FA/WA;
    flush 要挤出去的正是 FA 侧那半。取值优先级:
      --cache-buffer-capacity-gb > 环境变量 UCM_CACHE_BUFFER_CAPACITY_GB > 默认 64
    返回 FA 容量(GiB),同时写回 args。
    """
    arg_gb = args.cache_buffer_capacity_gb
    env_gb = env_float("UCM_CACHE_BUFFER_CAPACITY_GB", 0.0)
    if arg_gb > 0:
        total, src = arg_gb, "--cache-buffer-capacity-gb"
    elif env_gb > 0:
        total, src = env_gb, "环境变量 UCM_CACHE_BUFFER_CAPACITY_GB"
    else:
        total, src = DDR_FALLBACK_GB, f"默认值 {DDR_FALLBACK_GB:g}"
        print(f"   [warn] 未指定 DDR 容量(既无 --cache-buffer-capacity-gb,也无环境变量\n"
              f"          UCM_CACHE_BUFFER_CAPACITY_GB),按默认 {DDR_FALLBACK_GB:g}GB 推导。\n"
              f"          容量随部署不同,请显式指定 —— 否则推算出的池规模/压力条数会与实际不匹配。")
    fa = args.fa_buffer_gib if args.fa_buffer_gib > 0 else total / 2.0
    args.ddr_total_gb, args.fa_gib = total, fa
    print(f"   DDR 总量={total:g}GB(来源: {src}) -> FA/预取缓存={fa:g}GiB"
          f"  [按 hma 对半切;可用 --fa-buffer-gib 覆盖]")
    return fa


def ddr_need_bytes(args) -> float:
    """挤空 DDR 需要趟过的最小去重体量 = FA 容量 × 余量(默认 1.20)。"""
    return args.fa_gib * (1 << 30) * args.ddr_margin


def entries_for_fa(args, pool: dict) -> int:
    """压力素材条数 N:使 N × 单条体量 >= FA 容量 × 余量(与 T1 的组数同口径)。"""
    gb = statistics.median(e["bytes"] for e in pool["entries"])
    return max(1, math.ceil(ddr_need_bytes(args) / gb))


def group_bytes_of(args, pool: dict) -> float:
    """单个 flush "组"(一趟重放的那批条目)的 KV 体量(字节)。"""
    per_group = pool.get("group_entries") or entries_needed(args, pool)
    return per_group * statistics.median(e["bytes"] for e in pool["entries"])


def resolve_flush_plan(args, pool: dict) -> int:
    """按 DDR 容量推导 flush 规模,并把 --flush-passes/--pool-groups 填成实际值。

    口径:同一 flush 阶段连做 `passes` 趟,每趟重放**不同的组**(池游标推进),
    累计去重体量 = passes × 单组体量,必须 >= FA 容量 × 余量 —— 否则目标前缀
    (刚 [A] dump 进去的最新数据)留在 DDR 里,冷样本命中内存,测不出介质差。
    返回 0 表示满足;非 0 表示池太小,需要按当前 DDR 重新预放。
    """
    groups = pool.get("groups") or args.pool_groups or 1
    gb = group_bytes_of(args, pool)
    need = ddr_need_bytes(args)
    need_groups = max(1, math.ceil(need / gb))
    if args.pool_groups <= 0:
        args.pool_groups = groups
    print(f"   flush 规模: 单组 {gb / (1 << 30):.1f}GiB × 池 {groups} 组"
          f"  /  需 >= {need / (1 << 30):.1f}GiB(FA {args.fa_gib:g}GiB ×{args.ddr_margin:g})"
          f"  => 至少 {need_groups} 组")
    if groups < need_groups:
        print(f"[FAIL] 池只有 {groups} 组(合计 {groups * gb / (1 << 30):.1f}GiB),"
              f"挤不动 FA {args.fa_gib:g}GiB。请按当前 DDR 重新预放(一次性):\n"
              f"       python3 {os.path.basename(sys.argv[0])} --prepare-pool "
              f"--pool-file {args.pool_file} --pool-groups {need_groups}")
        return 1
    want = max(1, math.ceil(need / gb))
    if args.flush_passes <= 0:
        args.flush_passes = min(groups, want)
    elif args.flush_passes * gb < need:
        print(f"   [warn] --flush-passes {args.flush_passes} × 单组 {gb / (1 << 30):.1f}GiB"
              f" < 需要 {need / (1 << 30):.1f}GiB,可能挤不动 DDR")
    print(f"   flush 趟数={args.flush_passes}(累计 "
          f"{args.flush_passes * gb / (1 << 30):.1f}GiB/阶段)")
    return 0


def entries_needed(args, pool: dict) -> int:
    """一组需要的条数:覆盖 flush_gb 所需,向上取整,至少 2 条。"""
    per = statistics.median(e["bytes"] for e in pool["entries"])
    return max(2, math.ceil(args.flush_gb * (1 << 30) / per))


def prepare_pool(args, log_file) -> int:
    """预放 --pool-groups 组,每组 entries_needed() 条;按 nonce 幂等,可断点续传。"""
    pool_uid = None
    if os.path.exists(args.pool_file):
        pool = pool_load(args.pool_file)
        if pool.get("flush_tokens") != args.flush_tokens:
            print(f"[FAIL] 池文件 {args.pool_file} 的 flush_tokens={pool.get('flush_tokens')}"
                  f" 与当前 --flush-tokens={args.flush_tokens} 不一致,请换 --pool-file")
            return 1
        pool_uid = pool["uid"]
        have = {e["nonce"] for e in pool["entries"]}
        print(f"    续传现有池: 已有 {len(have)} 条 (uid={pool_uid})")
    else:
        pool_uid = f"{int(time.time()):x}-{random.randrange(1 << 24):06x}"
        pool = {"version": 1, "uid": pool_uid, "flush_tokens": args.flush_tokens,
                "entries": [], "cursor": 0}
        have = set()

    per_group = args.pool_group_entries  # 0 = 按单条大小自动计算
    probe_nonce = f"{pool_uid}-g0e0"
    probe = next((e for e in pool["entries"] if e["nonce"] == probe_nonce), None)
    if probe is None:
        print("    探测条目大小(第 1 条)...")
        s = chat(args.vllm_url, args.model, build_prompt(args.flush_tokens, probe_nonce),
                 args.max_tokens, args.timeout, session_id=f"pool-{probe_nonce}")
        if not s["block_ids"]:
            print("[FAIL] 预放前缀未返回 ucm_block_ids(prompt 太短?)")
            return 1
        probe = {"nonce": probe_nonce, "prompt_tokens": s["prompt_tokens"],
                 "blocks": len(s["block_ids"]),
                 "bytes": len(s["block_ids"]) * args.block_bytes}
        pool["entries"].append(probe)
        have.add(probe_nonce)
        pool_save(args.pool_file, pool)
        print(f"    {probe_nonce}: {probe['prompt_tokens']} tok / {probe['blocks']} blocks"
              f" / {probe['bytes'] / (1 << 30):.2f}GB / dump {s['elapsed_s']:.1f}s")
    if not per_group:
        per_group = max(2, math.ceil(args.flush_gb * (1 << 30) / probe["bytes"]))
    if args.pool_groups <= 0:
        # 按 DDR 容量推导需要几组:每群体量 = per_group × 单条体量
        per_bytes = per_group * probe["bytes"]
        args.pool_groups = max(1, math.ceil(ddr_need_bytes(args) / per_bytes))
        print(f"    --pool-groups 未指定:FA {args.fa_gib:g}GiB ×{args.ddr_margin:g}"
              f" 需 >= {ddr_need_bytes(args) / (1 << 30):.1f}GiB / 每组 "
              f"{per_bytes / (1 << 30):.1f}GiB => **{args.pool_groups} 组**")
    total = args.pool_groups * per_group
    print(f"    池规模: {args.pool_groups} 组 x {per_group} 条 = {total} 条"
          f" (每组 ~{per_group * probe['bytes'] / (1 << 30):.1f}GB)")

    def dump_one(gi: int, ei: int):
        nonce = f"{pool_uid}-g{gi}e{ei}"
        s = chat(args.vllm_url, args.model, build_prompt(args.flush_tokens, nonce),
                 args.max_tokens, args.timeout, session_id=f"pool-{nonce}")
        return gi, ei, nonce, s

    def save_entry(nonce: str, s: dict, done_cnt: int) -> None:
        pool["entries"].append({
            "nonce": nonce, "prompt_tokens": s["prompt_tokens"],
            "blocks": len(s["block_ids"]),
            "bytes": len(s["block_ids"]) * args.block_bytes,
        })
        have.add(nonce)
        pool_save(args.pool_file, pool)  # 每条落盘,断点续传
        print(f"    {nonce}: {s['prompt_tokens']:>7} tok / {len(s['block_ids']):>5} blocks"
              f" / {len(s['block_ids']) * args.block_bytes / (1 << 30):.2f}GB"
              f" / dump {s['elapsed_s']:5.1f}s  ({done_cnt}/{total})", flush=True)

    if args.pool_serial_wait > 0:
        # 串行验证模式:dump 异步落盘速率约 ~25-35MB/s(单条 1.49GB 需 ~50-60s),
        # 且 FA buffer 淘汰先于落盘完成时 block 永久丢失(容量越小越容易)。
        # 每条 dump 后等待并"重放确认"(load 命中=SSD 已有);未命中的重放本身
        # 就是重新 prefill+dump(自愈),最多重试 3 轮,自适应收敛。
        # 顺利条 ~75s(45s 等待 + 重放确认 + 重试余量),典型 34 条(2 组)约 40-45 分钟(一次性)。
        print(f"    串行验证模式: 每条 dump 后等待 {args.pool_serial_wait:.0f}s 并重放确认落盘")
        done_cnt = len(have)
        bad_serial: list[str] = []
        for gi in range(args.pool_groups):
            for ei in range(per_group):
                nonce = f"{pool_uid}-g{gi}e{ei}"
                if nonce in have:
                    continue
                gi2, ei2, nonce, s = dump_one(gi, ei)
                done_cnt += 1
                save_entry(nonce, s, done_cnt)
                ok = False
                for attempt in range(1, 4):
                    time.sleep(args.pool_serial_wait)
                    prompt = build_prompt(args.flush_tokens, nonce)
                    s2 = chat(args.vllm_url, args.model, prompt, args.max_tokens,
                              args.timeout,
                              session_id=f"pv-{nonce}-{random.randrange(1 << 16):04x}")
                    f2 = wait_fawa(log_file, s2["id"], args.log_wait_tries, args.log_wait_s)
                    if f2.get("load", 0) > 0 or f2.get("hbm", 0) > 0:
                        ok = True
                        print(f"      落盘确认 ok: replay {s2['elapsed_s']:5.1f}s"
                              f" load={f2.get('load', '?')}"
                              f" (第 {attempt} 次验证)", flush=True)
                        break
                    print(f"      第 {attempt} 次验证未命中(replay {s2['elapsed_s']:.0f}s,"
                          f" 已重新dump),再等 {args.pool_serial_wait:.0f}s...", flush=True)
                if not ok:
                    bad_serial.append(nonce)
                    print(f"      [warn] {nonce} 3 次验证均未命中,留给末尾校验轮处理", flush=True)
        if bad_serial:
            print(f"    [warn] 串行验证后仍未落盘: {len(bad_serial)} 条 {bad_serial}")
    else:
        with ThreadPoolExecutor(max_workers=args.flush_concurrency) as ex:
            pending = set()
            # have 里的条目已计入 done_cnt;迭代器跳过它们时不再重复计数
            # (否则补发条件提前满足,会漏跑最后一条)
            done_cnt = len(have)
            it = iter((gi, ei) for gi in range(args.pool_groups) for ei in range(per_group))
            while True:
                while done_cnt + len(pending) < total:
                    try:
                        gi, ei = next(it)
                    except StopIteration:
                        break
                    if f"{pool_uid}-g{gi}e{ei}" in have:
                        continue
                    pending.add(ex.submit(dump_one, gi, ei))
                if not pending:
                    break
                done, pending = wait(pending, return_when=FIRST_COMPLETED)
                for fut in done:
                    gi, ei, nonce, s = fut.result()
                    done_cnt += 1
                    save_entry(nonce, s, done_cnt)
    pool["group_entries"] = per_group
    pool["groups"] = args.pool_groups
    pool_save(args.pool_file, pool)
    print(f"\n[OK] 预放完成: {len(pool['entries'])} 条 -> {args.pool_file}")

    # 校验轮:逐条重放确认数据真的在外部缓存。高并发 dump 时 DDR 淘汰是
    # best-effort(connector 注释:"Dump failures only mean the external cache
    # may miss later"),早期条目可能被挤丢而未写回 SSD。未命中(hbm=0 且
    # load=0)的条目在本轮重放时会重新 prefill+dump(自愈),随后二次确认。
    def replay_entry_once(e: dict, tag: str):
        s = chat(args.vllm_url, args.model, build_prompt(args.flush_tokens, e["nonce"]),
                 args.max_tokens, args.timeout,
                 session_id=f"{tag}-{e['nonce']}-{random.randrange(1 << 16):04x}")
        f = wait_fawa(log_file, s["id"], args.log_wait_tries, args.log_wait_s)
        hit = f.get("hbm", 0) > 0 or f.get("load", 0) > 0
        return s, f, hit

    print(f"\n    校验轮(重放全池 {len(pool['entries'])} 条,未命中条目自愈重dump):")
    bad = []
    for e in pool["entries"]:
        s, f, hit = replay_entry_once(e, "poolverify")
        if not hit:
            bad.append(e)
        print(f"      {e['nonce'][-9:]}: {s['elapsed_s']:5.2f}s rank={s['rank']}"
              f"  hbm={f.get('hbm', '?')} ext={f.get('external', '?')}"
              f" load={f.get('load', '?')}"
              f"  ({'ok' if hit else '未命中->已重新dump'})", flush=True)
    if bad:
        print(f"    二次确认 {len(bad)} 条自愈条目...")
        still_bad = []
        for e in bad:
            s, f, hit = replay_entry_once(e, "poolverify2")
            print(f"      {e['nonce'][-9:]}: {s['elapsed_s']:5.2f}s"
                  f"  hbm={f.get('hbm', '?')} load={f.get('load', '?')}"
                  f"  ({'ok' if hit else '仍未命中!'})", flush=True)
            if not hit:
                still_bad.append(e["nonce"])
        if still_bad:
            print(f"    [warn] {len(still_bad)} 条二次校验仍未命中: {still_bad}")
            print("           这些条目重放时会退化为 prefill+dump(慢且不写 DDR),"
                  "建议换 --pool-file 重新 --prepare-pool")
        else:
            print(f"    [OK] 自愈成功,全部条目可用")
    else:
        print(f"    [OK] 全部 {len(pool['entries'])} 条一次校验通过")
    return 0


def replay_group(args, pool, log_file, tag: str) -> dict:
    """重放池中 cursor 指向的一组:同 nonce 重放 -> hash 命中 SSD -> external load
    进 DDR,把目标前缀挤出 FA buffer。返回统计;游标推进并持久化。

    "组"= 池条目列表按 group_entries 等分的批次(与 nonce 里的 g 编号无关,
    只需保证同批两次使用间隔 (groups-1) 批写入,即足以写穿 FA buffer)。
    """
    per_group = pool.get("group_entries") or entries_needed(args, pool)
    groups = pool.get("groups") or args.pool_groups
    start = pool["cursor"] % groups
    idxs = [(start * per_group + i) % len(pool["entries"]) for i in range(per_group)]
    print(f"\n[{tag}] flush(重放): 第 {start} 组 {per_group} 条"
          f"(SSD->DDR,目标挤出 FA buffer)")

    def replay_one(i: int, e: dict):
        # sid 带随机后缀:路由落点随机,降低落到"HBM 残留副本"的概率(DDR 共享,落点无碍)
        sid = f"poolflush-{e['nonce']}-{random.randrange(1 << 32):08x}"
        s = chat(args.vllm_url, args.model, build_prompt(args.flush_tokens, e["nonce"]),
                 args.max_tokens, args.timeout, session_id=sid)
        f = wait_fawa(log_file, s["id"], args.log_wait_tries, args.log_wait_s)
        return i, e, s, f

    stats = {"n": 0, "hbm_hit": 0, "no_load": 0, "load_blocks": 0, "elapsed": 0.0,
             "bytes": 0}
    with ThreadPoolExecutor(max_workers=args.flush_concurrency) as ex:
        pending = {ex.submit(replay_one, i, pool["entries"][i]) for i in idxs}
        while pending:
            done, pending = wait(pending, return_when=FIRST_COMPLETED)
            for fut in sorted(done, key=lambda x: x.result()[0]):
                i, e, s, f = fut.result()
                hbm = f.get("hbm")
                load = f.get("load", 0)
                if len(s["block_ids"]) != e["blocks"]:
                    print(f"    [warn] {e['nonce']} blocks {len(s['block_ids'])}"
                          f" != 预放 {e['blocks']},池数据可能异常")
                stats["n"] += 1
                stats["elapsed"] += s["elapsed_s"]
                if hbm is None:
                    note = "日志缺"
                elif hbm > 0:
                    stats["hbm_hit"] += 1
                    note = f"HBM命中{hbm}tok(不写DDR)"
                elif load == 0:
                    stats["no_load"] += 1
                    note = "load=0!前缀不在外部缓存?"
                else:
                    stats["load_blocks"] += load
                    stats["bytes"] += load * args.block_bytes
                    note = "SSD/DDR->DDR"
                print(f"    {tag} {e['nonce'][-9:]}: {s['elapsed_s']:5.2f}s rank={s['rank']}"
                      f"  hbm={hbm if hbm is not None else '?'} load={f.get('load', '?')}"
                      f"  [{note}]", flush=True)
    ext_gb = stats["bytes"] / (1 << 30)
    pool["cursor"] = (start + 1) % groups
    pool_save(args.pool_file, pool)
    print(f"    合计: {stats['n']} 条 / wall {stats['elapsed']:.1f}s"
          f" / external load {stats['load_blocks']} blocks = {ext_gb:.1f}GB"
          f" / HBM 命中 {stats['hbm_hit']} 条"
          + (f" / [WARN] {stats['no_load']} 条未加载!" if stats["no_load"] else ""))
    if stats["hbm_hit"]:
        print(f"    [warn] {stats['hbm_hit']}/{stats['n']} 条 HBM 命中(未写 DDR);"
              f"若 external load 总量仍 >= FA 容量({args.fa_gib:g}GiB),挤出依然成立")
    return stats


def main() -> int:
    ap = argparse.ArgumentParser(description="UCM 预取效果对照测试(TTFT,重放池快速版)")
    ap.add_argument("--vllm-url", default=os.environ.get("UCM_ROUTER_URL",
                                                         "http://127.0.0.1:9003"),
                    help="vllm-router 地址(推理与预取共用该入口)")
    ap.add_argument("--model", default="deepseek")
    ap.add_argument("--prompt-tokens", type=int, default=32768, help="被测前缀长度")
    ap.add_argument("--prompt-tokens-list", default="",
                    help="逗号分隔的多档长度(如 8192,32768,65536,131072)")
    ap.add_argument("--prefixes", type=int, default=5, help="每档长度的被测前缀个数(K)")
    ap.add_argument("--warmup-prefixes", type=int, default=4,
                    help="牺牲前缀个数:吃掉空闲后首次外部加载的一次性开销")
    ap.add_argument("--warmup-tokens", type=int, default=8192, help="牺牲前缀长度")
    ap.add_argument("--flush-tokens", type=int, default=262144, help="flush 用前缀长度")
    ap.add_argument("--flush-gb", type=float, default=24.0,
                    help="单组目标 KV 量(GiB)。组体量随它在池里被固化为 group_entries 条")
    ap.add_argument("--flush-passes", type=int, default=0,
                    help="每个 flush 阶段连续重放几组(0=按 DDR 容量自动推导,推荐)。"
                         "必须满足 passes × 单组体量 >= FA 容量 × --ddr-margin,"
                         "否则目标前缀挤不出 DDR(冷样本会命中内存)")
    ap.add_argument("--flush-concurrency", type=int, default=4,
                    help="flush/预放并发提交条数,建议 <= DP size")
    ap.add_argument("--block-bytes", type=int, default=BLOCK_BYTES_DEFAULT)
    ap.add_argument("--max-tokens", type=int, default=1)
    ap.add_argument("--retries", type=int, default=6)
    ap.add_argument("--warm-chunk", type=int, default=128)
    ap.add_argument("--timeout", type=float, default=900.0)
    ap.add_argument("--log-dir", default=LOG_DIR_DEFAULT)
    ap.add_argument("--log-wait-tries", type=int, default=8)
    ap.add_argument("--log-wait-s", type=float, default=0.5)
    ap.add_argument("--no-drop-caches", action="store_true")
    ap.add_argument("--skip-flush", action="store_true", help="跳过 flush(调试用)")
    # DDR 容量:随部署不同,与容量挂钩的参数(flush 趟数/池组数)都按它推导
    ap.add_argument("--cache-buffer-capacity-gb", type=float, default=0.0,
                    help="UCM 的 cache_buffer_capacity_gb(**节点级** DDR 缓存总量,GiB)。"
                         "0=读环境变量 UCM_CACHE_BUFFER_CAPACITY_GB,再不行用 64。"
                         "换部署只需改这里/设环境变量")
    ap.add_argument("--fa-buffer-gib", type=float, default=0.0,
                    help="FA(预取缓存)侧容量 GiB;0=总量对半切(hma_connector 默认行为)")
    ap.add_argument("--ddr-margin", type=float, default=1.20,
                    help="挤 DDR 的余量系数:要求 累计去重体量 >= FA 容量 × 该值(默认 1.20)")
    # 池相关
    ap.add_argument("--pool-file", default=POOL_FILE_DEFAULT, help="预放池 JSON 路径")
    ap.add_argument("--pool-groups", type=int, default=0,
                    help="池组数(0=按 DDR 容量自动推导,推荐)。组数决定 flush 能连续"
                         "写穿 DDR 的上限:passes 趟必须落在不同的组上")
    ap.add_argument("--pool-group-entries", type=int, default=0,
                    help="每组条数(默认按 --flush-gb 自动计算)")
    ap.add_argument("--pool-serial-wait", type=float, default=0.0,
                    help="预放串行验证模式(>0 启用):每条 dump 后等待 N 秒并重放确认落盘,"
                    "未命中自动重试(自愈)。dump 异步落盘仅 ~25-35MB/s,写入快于落盘且"
                    "FA buffer 淘汰时数据即丢,故首次预放建议开启。"
                    "耗时:典型 34 条(2 组)约 40-45 分钟;不开(默认 0)则不校验,约 20-25 分钟。建议 45")
    ap.add_argument("--prepare-pool", action="store_true",
                    help="只做一次性预放(幂等,可断点续传)")
    ap.add_argument("--replay-one-group", action="store_true",
                    help="只重放一组并打印 FAWA 观测(不跑测试)")
    ap.add_argument("--out-dir", default=OUT_DIR_DEFAULT,
                    help="结果 json 的输出目录(默认 <项目根>/reports/data/t1)")
    ap.add_argument("--out-json", default="",
                    help="结果 json 的完整路径;留空则用 <out-dir>/tier_gain_<run_id>.json")
    args = ap.parse_args()

    run_id = f"{int(time.time())}-{random.randrange(1 << 24):06x}"
    log_file = latest_vllm_log(args.log_dir)
    resolve_ddr(args)   # DDR 容量 -> FA 侧容量(与容量挂钩的参数都按它推导)

    if args.prepare_pool:
        print(f"== 预放 flush 池  run={run_id}  -> {args.pool_file}")
        print(f"   router={args.vllm_url}  组数={args.pool_groups}"
              f"  单条={args.flush_tokens}tok  并发={args.flush_concurrency}")
        print(f"   worker 日志: {log_file}")
        return prepare_pool(args, log_file)

    if not os.path.exists(args.pool_file):
        print(f"[FAIL] 池文件不存在: {args.pool_file}\n"
              f"       先运行: python3 {sys.argv[0]} --prepare-pool")
        return 1
    pool = pool_load(args.pool_file)
    if pool.get("flush_tokens") != args.flush_tokens:
        print(f"[FAIL] 池 flush_tokens={pool.get('flush_tokens')} != {args.flush_tokens}")
        return 1
    if resolve_flush_plan(args, pool):
        return 1

    if args.replay_one_group:
        print(f"== 重放一组(观测)  run={run_id}  池={args.pool_file}"
              f" ({len(pool['entries'])} 条, cursor={pool['cursor']})")
        replay_group(args, pool, log_file, "R")
        return 0

    print(f"== UCM 预取效果对照测试(快速版)  run={run_id}")
    print(f"   vllm-router={args.vllm_url}  预取入口=/kv/prefetch")
    lengths = sorted({int(x) for x in args.prompt_tokens_list.split(",") if x.strip()}) \
        or [args.prompt_tokens]
    print(f"   长度档={[f'{L // 1024}k' for L in lengths]}  每档 K={args.prefixes}  "
          f"flush=重放池({args.flush_gb}GB/次,读路径)  "
          f"牺牲={args.warmup_prefixes}x{args.warmup_tokens}tok")
    print(f"   单block={args.block_bytes}B  drop_caches={'off' if args.no_drop_caches else 'on'}"
          f"  池={len(pool['entries'])}条/{args.pool_groups}组 cursor={pool['cursor']}")
    print(f"   worker 日志: {log_file}")

    # ---- [A] dump:让每个前缀的 KV 落到 DDR+SSD ----
    print("\n[A] dump:每个前缀跑一次,记录 block_ids / prefetch_rank")
    prefixes = []
    for L in lengths:
        for i in range(args.prefixes):
            name = f"{L // 1024}k#{i}"
            sid = f"{run_id}-{L}-p{i}"
            prompt = build_prompt(L, f"{run_id}-{L}-p{i}")
            s = chat(args.vllm_url, args.model, prompt, args.max_tokens, args.timeout,
                     session_id=sid)
            if not s["block_ids"]:
                print(f"[FAIL] {name} 未返回 ucm_block_ids(prompt < dump 阈值?)")
                return 1
            prefixes.append({"name": name, "len": L, "prompt": prompt, "sid": sid, "dump": s})
            print(f"    {name:<9} {s['prompt_tokens']:>7} tok / {len(s['block_ids']):>5} blocks"
                  f" / rank={s['rank']} / dump {s['elapsed_s']:5.1f}s", flush=True)

    sacrifice = []
    for i in range(args.warmup_prefixes):
        sid = f"{run_id}-w{i}"
        prompt = build_prompt(args.warmup_tokens, f"{run_id}-w{i}")
        s = chat(args.vllm_url, args.model, prompt, args.max_tokens, args.timeout,
                 session_id=sid)
        sacrifice.append({"name": f"W{i}", "prompt": prompt, "sid": sid, "dump": s})
        print(f"    W{i}: {s['prompt_tokens']:>7} tok / {len(s['block_ids']):>5} blocks"
              f" / rank={s['rank']} / dump {s['elapsed_s']:5.1f}s  (牺牲样本)", flush=True)

    # ---- [B] flush#1:重放池一组,把目标前缀挤出 DDR ----
    # 未命中(load=0)条目走 prefill+dump 慢路径:照样写 DDR 占位,flush 依然有效,
    # 只是该条 ~28s vs 命中 ~2s。数据不持久是 UCM 侧已知行为:连续大量 dump 时
    # 异步持久化被淹没,空闲期 dump 才可靠落盘(实验验证:空闲 dump+120s 后全命中)。
    if not args.skip_flush:
        for k in range(1, args.flush_passes + 1):
            tag = f"B-flush#1.{k}" if args.flush_passes > 1 else "B-flush#1"
            st = replay_group(args, pool, log_file, tag)
            if st["no_load"]:
                print(f"    [warn] {st['no_load']} 条未命中走慢路径(prefill+dump,仍占 DDR,flush 有效)")
    if not args.no_drop_caches:
        print(f"\n    清 page cache(保险,io_direct=true 时本不必要): "
              f"{'成功' if drop_caches() else '失败'}")

    # ---- [B2] 一次性开销预热:牺牲前缀回源,不计入结果 ----
    print("\n[B2] 牺牲前缀:吸收首次外部加载的一次性开销(结果不计入)")
    for w in sacrifice:
        s = chat(args.vllm_url, args.model, w["prompt"], args.max_tokens, args.timeout,
                 session_id=w["sid"])
        f = wait_fawa(log_file, s["id"], args.log_wait_tries, args.log_wait_s)
        print(f"    {w['name']}: {s['elapsed_s']:6.2f}s  rank={s['rank']}"
              f"  hbm={f.get('hbm', '?')} load={f.get('load', '?')}", flush=True)

    # ---- [C] cold:SSD-only ----
    print("\n[C] cold:不预热,直接跑(源应为 SSD)")
    cold = []
    for p in prefixes:
        s = measure(args, p["prompt"], p["dump"]["rank"], p["name"], log_file, sid=p["sid"])
        if s is None:
            print(f"[FAIL] {p['name']} 连续 {args.retries} 次都命中 HBM,无法取得冷样本")
            return 1
        p["cold"] = s
        cold.append(s["elapsed_s"])

    # ---- [D] flush#2:重放池下一组,清掉 [C] 读回 DDR 的内容 ----
    if not args.skip_flush:
        for k in range(1, args.flush_passes + 1):
            tag = f"D-flush#2.{k}" if args.flush_passes > 1 else "D-flush#2"
            st = replay_group(args, pool, log_file, tag)
            if st["no_load"]:
                print(f"    [warn] {st['no_load']} 条未命中走慢路径(prefill+dump,仍占 DDR,flush 有效)")

    # ---- [E] warm:手动预热 ----
    print("\n[E] warm:手动调 router /kv/prefetch(session 键与测量请求一致)")
    total_blocks = 0
    warm_secs: list[float] = []
    for p in prefixes:
        r = prefetch_warm(args.vllm_url, p["dump"]["block_ids"], args.warm_chunk,
                          args.timeout, headers={"x-session-id": p["sid"]})
        total_blocks += len(p["dump"]["block_ids"])
        warm_secs.append(r["elapsed_s"])
        print(f"    {p['name']}: {len(p['dump']['block_ids'])} blocks -> prefetch_count="
              f"{r['prefetch_count']} mode={r['modes']} dp_rank={r['dp_ranks']}"
              f"  warm {r['elapsed_s']:.2f}s", flush=True)
    print(f"    预热合计 {sum(warm_secs):.2f}s / {len(warm_secs)} 前缀 / {total_blocks} blocks")

    # ---- [F] hot:DDR 回源 ----
    print("\n[F] hot:预热后再跑(源应为 DDR)")
    hot = []
    for p in prefixes:
        s = measure(args, p["prompt"], p["dump"]["rank"], p["name"], log_file, sid=p["sid"])
        if s is None:
            print(f"[warn] {p['name']} 只取到 HBM 命中样本,跳过")
            continue
        p["hot"] = s
        hot.append(s["elapsed_s"])

    if not hot:
        print("[FAIL] 无有效热样本")
        return 1

    # ---- 汇总 ----
    print("\n== 结果(TTFT 近似值:max_tokens=1 非流式 wall-clock)")
    print(f"{'前缀':<10}{'cold(SSD)':>12}{'hot(DDR)':>12}{'加速':>8}   FAWA(hbm/ext/load)")
    for p in prefixes:
        if "cold" not in p or "hot" not in p:
            continue
        c, h = p["cold"], p["hot"]
        f_c, f_h = c.get("fawa", {}), h.get("fawa", {})
        fmt = lambda f: f"{f.get('hbm', '?')}/{f.get('external', '?')}/{f.get('load', '?')}"
        speed = c["elapsed_s"] / h["elapsed_s"] if h["elapsed_s"] else 0
        print(f"{p['name']:<10}{c['elapsed_s']:>11.2f}s{h['elapsed_s']:>12.2f}s{speed:>7.2f}x"
              f"   {fmt(f_c)}  ->  {fmt(f_h)}")

    print("\n== 按上下文长度汇总(每档中位)")
    print(f"{'长度':<8}{'blocks':>8}{'KV(MB)':>9}{'cold(SSD)':>11}{'hot(DDR)':>10}"
          f"{'省(s)':>8}{'加速':>8}{'省/MB(ms)':>11}")
    for L in lengths:
        g = [p for p in prefixes if p.get("len") == L and "cold" in p and "hot" in p]
        if not g:
            continue
        cm = statistics.median([p["cold"]["elapsed_s"] for p in g])
        hm = statistics.median([p["hot"]["elapsed_s"] for p in g])
        nb = statistics.median([len(p["dump"]["block_ids"]) for p in g])
        mb = nb * args.block_bytes / (1 << 20)
        per_mb = (cm - hm) / mb * 1e3 if mb else 0.0
        print(f"{f'{L // 1024}k':<8}{nb:>8.0f}{mb:>9.0f}{cm:>10.2f}s{hm:>9.2f}s"
              f"{cm - hm:>8.2f}{cm / hm:>7.2f}x{per_mb:>11.2f}")

    cm, hm = statistics.median(cold), statistics.median(hot)
    print(f"\n全体中位 TTFT: cold={cm:.2f}s  hot={hm:.2f}s  差值={cm - hm:.2f}s  加速={cm / hm:.2f}x")
    # 过滤 None(rank 缺失的样本)再排序,避免对含 None 的集合排序报 TypeError
    c_ranks = sorted({p["cold"]["rank"] for p in prefixes
                      if "cold" in p and p["cold"]["rank"] is not None})
    h_ranks = sorted({p["hot"]["rank"] for p in prefixes
                      if "hot" in p and p["hot"]["rank"] is not None})
    print(f"冷样本 rank 分布: {c_ranks}")
    print(f"热样本 rank 分布: {h_ranks}")

    hbm_leak = [p["name"] for p in prefixes
                if p.get("cold", {}).get("fawa", {}).get("hbm", 0) > 0]
    if hbm_leak:
        print(f"[warn] 冷样本仍命中 HBM(结论存疑): {hbm_leak}")
    if any(p.get("cold", {}).get("fawa", {}).get("load", 0) == 0
           for p in prefixes if "cold" in p):
        print("[warn] 存在 load blocks=0 的冷样本,说明请求根本没走外部加载")

    passed = hm < cm * 0.9
    summary = build_result(args, run_id, prefixes, lengths, cold, hot,
                           total_blocks, warm_secs, c_ranks, h_ranks,
                           hbm_leak, passed)
    write_result(args, run_id, summary)
    if passed:
        print(f"\n[PASS] 预取有效:{cm / hm:.2f}x 加速(冷 {cm:.2f}s -> 热 {hm:.2f}s)")
        return 0
    print(f"\n[FAIL] 未观察到明显收益({cm:.2f}s -> {hm:.2f}s)。"
          f"若 flush 重放的 HBM 命中较多,见上方 [warn];"
          f"或加大 --flush-gb / 增大池组数后重试")
    return 1


def build_result(args, run_id, prefixes, lengths, cold, hot, total_blocks,
                 warm_secs, c_ranks, h_ranks, hbm_leak, passed):
    """把本轮结果整理成结构化 json(供后续分析消费)。"""
    tiers = []
    for L in lengths:
        g = [p for p in prefixes if p.get("len") == L and "cold" in p and "hot" in p]
        if not g:
            continue
        cm = statistics.median([p["cold"]["elapsed_s"] for p in g])
        hm = statistics.median([p["hot"]["elapsed_s"] for p in g])
        nb = statistics.median([len(p["dump"]["block_ids"]) for p in g])
        mb = nb * args.block_bytes / (1 << 20)
        tiers.append({"tokens": L, "n": len(g), "blocks": int(nb),
                      "kv_mb": round(mb, 1),
                      "cold_s": round(cm, 3), "hot_s": round(hm, 3),
                      "saved_s": round(cm - hm, 3),
                      "speedup": round(cm / hm, 3) if hm else None,
                      "saved_ms_per_mb": round((cm - hm) / mb * 1e3, 3) if mb else None})
    cm = statistics.median(cold)
    hm = statistics.median(hot)
    return {
        "tier": "t1_media_gain",
        "run_id": run_id,
        "ts": time.time(),
        "ts_str": time.strftime("%Y-%m-%d %H:%M:%S", time.localtime()),
        "verdict": "PASS" if passed else "FAIL",
        # 判定口径:全体中位 cold(SSD) / hot(DDR) 的加速比 >= 1.11x(即 hot < cold*0.9)
        "cold_median_s": round(cm, 3),
        "hot_median_s": round(hm, 3),
        "saved_median_s": round(cm - hm, 3),
        "speedup_median": round(cm / hm, 3) if hm else None,
        "n_samples": len(cold),
        "tiers": tiers,
        "cold_ranks": c_ranks,
        "hot_ranks": h_ranks,
        "hbm_leak_cold": hbm_leak,
        "warmup_blocks": total_blocks,
        "warmup_total_s": round(sum(warm_secs), 3),
        "args": {k: v for k, v in vars(args).items()
                 if isinstance(v, (int, float, str, bool)) or v is None},
    }


def write_result(args, run_id, summary):
    path = args.out_json or os.path.join(args.out_dir, f"tier_gain_{run_id}.json")
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        json.dump(summary, f, ensure_ascii=False, indent=1)
    print(f"   结果 json: {path}")


if __name__ == "__main__":
    sys.exit(main())
