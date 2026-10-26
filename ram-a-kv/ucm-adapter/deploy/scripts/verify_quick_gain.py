#!/usr/bin/env python3
# -*- coding: utf-8 -*-
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

"""UCM 预取收益快速 A/B(**单卡视角**,两臂合计 ~9 分钟):一张卡上 N 个并发 agent 会话,预取开/关。
"""
from __future__ import annotations
import argparse
import glob
import json
import math
import os
import random
import re
import shutil
import statistics
import sys
import threading
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from verify_prefetch_effect_fast import (  # noqa: E402
    build_prompt, chat, prefetch_warm, drop_caches, scan_fawa,
    pool_load, BLOCK_BYTES_DEFAULT, LOG_DIR_DEFAULT,
    resolve_ddr, ddr_need_bytes, entries_for_fa,
)

HBM_TOKENS_PER_RANK = 2083204          # vLLM KV pool,每 DP rank(TP4/DP4 默认口径;
                                       #  TP8/DP1 为 558665,用 --hbm-pool-tokens 覆盖)
UCM_TOKENS_PER_BLOCK = 128             # UCM block = 128 tokens(FAWA 的 load 以 block 计)
EVICT_DOSE_X = 4.0                     # 挤出 HBM 所需的最小总投递倍数(× HBM 池);
                                       #  另需压力把池子占满;DP=1 请见 persist_probe
METRIC_NAMES = ("cache_load_shards_total", "cache_load_wait_shards_total",
                "cache_load_success_shards_total",
                "cache_posix_load_success_shards_total")
# /metrics 已知缺陷(curl/urllib 一致):同一 URL 连打,约 2/8 概率
# 返回 **138,560 B 的截断响应**——里面 0 条 `ucm:*`、也没有 `vllm:kv_cache_usage_perc` /
# `num_requests_running` / `num_requests_waiting_by_reason`(仅 1428 条无关 vllm 序列);
# 其余约 6/8 返回 1,269,637 B 完整响应(9998 条 ucm 行)。不做间隔地连续 80 次连打时
# 截断率升到 53~62%,且成串出现(不是独立同分布)。请求头(Connection: close)、
# 代理、编码都不是原因,纯粹是 engine 侧序列化竞态。后果:分层归因 ddr_ratio 变 null、
# 池峰值变空。故取数必须**校验目标家族存在**,不满足就**立刻重试**(不 sleep,以免把
# before/after 差分窗口拉宽、混进别会话的 shards)。8 次立即重试即可拿到
# 完整响应、平均 0.35s;加 60~150ms sleep 反而更慢,故不加。
METRICS_RETRIES = 8
METRICS_FAMILY_UCM = "ucm:cache_load_shards_total"
METRICS_FAMILY_ENGINE = "vllm:kv_cache_usage_perc"
REPORTS_DIR_DEFAULT = (os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
                       + "/reports/data/t2")
POOL_G1_DEFAULT = "/mnt/sdb/ucm/flush_pool_64g.json"


# ===== 通用小工具 =====

def log_files_for(log_dir: str, keep: int = 2) -> list:
    """worker 日志按 mtime 倒序取最近几个(FAWA 行可能在旧文件里)。"""
    files = glob.glob(os.path.join(log_dir, "vllm-*.log"))
    files.sort(key=os.path.getmtime, reverse=True)
    return files[:keep]


def wait_fawa_any(log_files, request_id, tries, interval) -> dict:
    """在多个日志文件里找该请求的 FAWA 行。"""
    for _ in range(max(1, tries)):
        for lf in log_files:
            hit = scan_fawa(lf, {request_id})
            if hit:
                return hit.get(request_id, {})
        time.sleep(interval)
    return {}


def tail_lines(path: str, nbytes: int = 262144) -> list:
    try:
        with open(path, "rb") as f:
            f.seek(0, os.SEEK_END)
            size = f.tell()
            f.seek(max(0, size - nbytes))
            data = f.read().decode("utf-8", "replace")
        return data.splitlines()
    except OSError:
        return []


def bootstrap_median_ci(values, iters=1000, lo=2.5, hi=97.5, seed=12345):
    """对中位数做 bootstrap 百分位区间。"""
    if not values:
        return None, None
    rnd = random.Random(seed)
    n = len(values)
    meds = []
    for _ in range(iters):
        meds.append(statistics.median([rnd.choice(values) for _ in range(n)]))
    meds.sort()
    return meds[max(0, int(iters * lo / 100.0))], meds[min(iters - 1, int(iters * hi / 100.0))]


def med(values):
    return statistics.median(values) if values else None


# ===== 分层归因:Prometheus /metrics 差分 =====

def fetch_metrics_text(url: str, marker: str, timeout: float = 15.0,
                       tries: int = METRICS_RETRIES):
    """取 /metrics 原文,直到正文里出现 marker 族为止(见文件头 METRICS_RETRIES 注释)。

    返回 None 表示 tries 次都没拿到含目标族的响应。重试**不 sleep**:目标是把
    retry 代价压到一次 RTT 量级,否则 caller 的 before/after 窗口会被拉宽。
    """
    for _ in range(max(1, tries)):
        try:
            with urllib.request.urlopen(url, timeout=timeout) as resp:
                text = resp.read().decode("utf-8", "replace")
        except Exception:  # noqa: BLE001
            continue
        if marker in text:
            return text
    return None


def metrics_snapshot(url: str):
    """{rank: {metric: value}};同一 engine 的 4 个 TP worker 数值相同,取 worker_rank 最小者。"""
    text = fetch_metrics_text(url, METRICS_FAMILY_UCM)
    if text is None:
        print(f"    [warn] metrics 拉取失败/连续 {METRICS_RETRIES} 次均为截断响应: {url}")
        return None
    picked: dict = {}
    for line in text.splitlines():
        if not line.startswith("ucm:"):
            continue
        head, _, rest = line.partition("{")
        labels, _, val = rest.partition("}")
        short = head.split(":", 1)[1].strip()
        if short not in METRIC_NAMES:
            continue
        lab = dict(re.findall(r'(\w+)="([^"]*)"', labels))
        if "engine" not in lab:
            continue
        try:
            rank = int(lab["engine"])
            wr = int(lab.get("worker_rank", "0"))
            value = float(val.strip())
        except ValueError:
            continue
        cur = picked.setdefault(rank, {}).get(short)
        if cur is None or wr < cur[0]:
            picked[rank][short] = (wr, value)
    return {r: {k: v[1] for k, v in d.items()} for r, d in picked.items()}


def metrics_delta(before, after) -> dict:
    """{rank: {d_<metric>, ddr_ratio, ssd_ratio}}。只在快照之间只有一个请求时才有意义。"""
    if not before or not after:
        return {}
    out = {}
    for rank, av in after.items():
        bv = before.get(rank) or {}
        row = {}
        for name in METRIC_NAMES:
            row["d_" + name] = av.get(name, 0.0) - bv.get(name, 0.0)
        total = row["d_cache_load_shards_total"]
        wait = row["d_cache_load_wait_shards_total"]
        row["ddr_ratio"] = (total - wait) / total if total > 0 else None
        row["ssd_ratio"] = wait / total if total > 0 else None
        out[rank] = row
    return out


# ===== HBM 池饱和度监控 =====

def pool_sample(url: str) -> dict:
    """取每 engine 的 HBM(vLLM KV pool)饱和度峰值所需的三个量。

    usage_perc 在 V1 口径下 = 1 - 空闲块/总块(缓存块算空闲),所以它逼近 1 才说明
    "在飞请求把池子占满",此时新分配必然复用(淘汰)缓存块。

    取数走 fetch_metrics_text(带家族校验+立即重试),否则截断响应会让池峰值整段为空。
    """
    text = fetch_metrics_text(url, METRICS_FAMILY_ENGINE, timeout=10.0)
    if text is None:
        return {}
    out: dict = {}
    for line in text.splitlines():
        if not line.startswith("vllm:"):
            continue
        head, _, rest = line.partition("{")
        labels, _, val = rest.partition("}")
        lab = dict(re.findall(r'(\w+)="([^"]*)"', labels))
        engine = lab.get("engine")
        if engine is None:
            continue
        try:
            rank, value = int(engine), float(val.strip())
        except ValueError:
            continue
        row = out.setdefault(rank, {})
        name = head.split(":", 1)[1]
        if name == "kv_cache_usage_perc":
            row["usage"] = max(row.get("usage", 0.0), value)
        elif name == "num_requests_running":
            row["running"] = max(row.get("running", 0.0), value)
        elif name == "num_requests_waiting_by_reason" and lab.get("reason") in (
                "capacity", "deferred"):
            key = "wait_" + lab["reason"]
            row[key] = max(row.get(key, 0.0), value)
    return out


class PoolMonitor:
    """压力期间后台采样,取每 engine 的池占用峰值。"""

    def __init__(self, url: str, interval: float = 0.5):
        self.url, self.interval = url, interval
        self.peak: dict = {}
        self._stop = threading.Event()
        self._th = None

    def _loop(self):
        while not self._stop.is_set():
            for r, row in pool_sample(self.url).items():
                cur = self.peak.setdefault(r, {})
                for k, v in row.items():
                    cur[k] = max(cur.get(k, 0.0), v)
            self._stop.wait(self.interval)

    def __enter__(self):
        self._th = threading.Thread(target=self._loop, daemon=True)
        self._th.start()
        return self

    def __exit__(self, *exc):
        self._stop.set()
        if self._th:
            self._th.join(timeout=5.0)
        return False


# ===== 请求封装 =====

def do_chat(args, prompt, sid, phase=""):
    """一次 chat + 等 FAWA 行。返回 (s, fawa)。

    重试节流
    --------
    router(`vllm-router`)自带断路器和健康检查：压力相位把引擎打满时，`/health` 会被
    拖到超过 `--health-check-timeout-secs`(默认 5s)，worker 被标记 unhealthy；断路器
    则在窗口内累计 `--cb-failure-threshold`(默认 10)次失败后 `closed -> open`。
    打开后 router 对**所有**请求直接回 503
    `No available workers (all circuits open or unhealthy)` —— 与 UCM、与被测引擎都无关。

    观测:历史上一轮曾因断路器打开导致该轮 12 个驱动请求全部 503、被测轮无样本而作废。

    因此退避总时长必须 **> 断路器 open 时长(默认 60s) + 健康检查恢复时间**
    (默认每 60s 一次、连续 3 次成功才恢复 ⇒ 最多 ~180s)。下面 8 次退避累计 ≈257s。
    """
    backoff = (2.0, 5.0, 10.0, 20.0, 40.0, 60.0, 60.0, 60.0)   # 累计 ≈257s
    last = None
    for attempt, wait in enumerate(backoff, start=1):
        try:
            s = chat(args.vllm_url, args.model, prompt, args.max_tokens, args.timeout,
                     session_id=sid)
        except Exception as e:  # noqa: BLE001
            last = e
            tag = ""
            if "503" in str(e):
                tag = "(router 断路器/健康检查瞬态,与 UCM 无关;退避后重试)"
            print(f"    [warn] {phase} chat 第 {attempt}/{len(backoff)} 次失败{tag}: {e}")
            time.sleep(wait)
            continue
        f = wait_fawa_any(args.log_files, s["id"], args.log_wait_tries, args.log_wait_s)
        return s, f
    raise RuntimeError(f"{phase} chat 连续失败({len(backoff)} 次退避已用尽): {last}")


def fawa_fields(f: dict) -> dict:
    return {
        "hbm": f.get("hbm"),
        "external": f.get("external"),
        "load": f.get("load"),
        "dump": f.get("dump"),
        "total": f.get("total"),
    }


class Recorder:
    """stdout 同步 + jsonl 实时落盘。"""

    def __init__(self, path):
        self.path = path
        self.f = open(path, "w", encoding="utf-8")
        self.records: list = []
        self._lk = threading.Lock()

    def add(self, **kw):
        kw["ts"] = round(time.time(), 3)
        with self._lk:
            self.records.append(kw)
            self.f.write(json.dumps(kw, ensure_ascii=False) + "\n")
            self.f.flush()

    def close(self):
        try:
            self.f.close()
        except OSError:
            pass


# ===== 体检 =====

def preflight(args) -> int:
    print("== 只读体检(preflight)")
    ok = True
    router_health = args.vllm_url.rstrip("/") + "/health"
    engine_health = args.metrics_url.rsplit("/metrics", 1)[0] + "/health"
    for name, url in (("router", router_health), ("engine", engine_health)):
        try:
            with urllib.request.urlopen(url, timeout=10) as resp:
                body = resp.read().decode("utf-8", "replace").strip()
            print(f"  [ok] {name} {url} -> HTTP {resp.status} {body[:60]}")
        except Exception as e:  # noqa: BLE001
            print(f"  [FAIL] {name} {url} -> {e}")
            ok = False

    snap = metrics_snapshot(args.metrics_url)
    if not snap:
        print(f"  [FAIL] metrics 不可用/无 ucm:* 序列: {args.metrics_url}")
        ok = False
    else:
        missing = [n for n in METRIC_NAMES if not any(n in v for v in snap.values())]
        ranks = sorted(snap)
        print(f"  [{'ok' if not missing else 'FAIL'}] metrics ranks={ranks}"
              f" 缺失序列={missing or '无'}")
        print(f"       样本: rank {ranks[0] if ranks else '-'} -> "
              f"{ {k: v for k, v in (snap.get(ranks[0]) if ranks else {}).items()} }")
        ok = ok and not missing and set(range(args.dp_size)).issubset(set(ranks))

    if not os.path.exists(args.pool_file):
        print(f"  [FAIL] 池文件不存在: {args.pool_file}")
        return 1
    pool = pool_load(args.pool_file)
    match = pool.get("flush_tokens") == args.flush_tokens
    print(f"  [{'ok' if match else 'FAIL'}] 池 {args.pool_file}: {len(pool['entries'])} 条"
          f" groups={pool.get('groups')} group_entries={pool.get('group_entries')}"
          f" flush_tokens={pool.get('flush_tokens')}(脚本 {args.flush_tokens}) cursor={pool.get('cursor')}")
    ok = ok and match and len(pool["entries"]) > 0

    print("  抽 3 条池条目重放确认外部缓存仍有效(纯读)...")
    lf = log_files_for(args.log_dir)
    entries = pool["entries"][:3] if len(pool["entries"]) <= 3 else random.sample(pool["entries"], 3)
    alive = 0
    for e in entries:
        try:
            s, f = do_chat(args, build_prompt(args.flush_tokens, e["nonce"]),
                           f"preflight-{e['nonce']}-{random.randrange(1 << 32):08x}",
                           phase="preflight")
        except Exception as ex:  # noqa: BLE001
            print(f"    [warn] {e['nonce'][-9:]}: {ex}")
            continue
        hit = (f.get("hbm") or 0) > 0 or (f.get("load") or 0) > 0
        alive += 1 if hit else 0
        print(f"    {e['nonce'][-9:]}: {s['elapsed_s']:5.2f}s rank={s['rank']}"
              f"  hbm={f.get('hbm', '?')} load={f.get('load', '?')}  [{'ok' if hit else '未命中!'}]")
    if alive < len(entries):
        print(f"  [warn] {len(entries) - alive}/{len(entries)} 条池条目未命中,"
              f"将退化为 prefill+dump(压力有效但慢)")
    ok = ok and alive > 0

    du = shutil.disk_usage("/mnt/sdb")
    print(f"  [{'ok' if du.free > 200 * (1 << 30) else 'warn'}] /mnt/sdb 余 "
          f"{du.free / (1 << 30):.0f}GB / 共 {du.total / (1 << 30):.0f}GB")
    for sub in ("fawa_fa", "fawa_wa"):
        p = f"/mnt/sdb/ucm/dsv4-kv/{sub}"
        print(f"       {p}: {'可写' if os.access(p, os.W_OK) else '不可写/不存在'}")

    if lf:
        gc = [ln for ln in tail_lines(lf[0]) if "GC sampling" in ln]
        for ln in gc[-3:]:
            print(f"  GC 采样: {ln.strip()[-160:]}")
    print(f"  [{'PASS' if ok else 'FAIL'}] preflight 结束")
    return 0 if ok else 1


# ===== 压力步 =====

def pressure_dose(args_lt, sessions, deliv) -> dict:
    """按 rank 汇总压力剂量,判断 HBM 是否翻得动。

    剂量口径是**累计投递量 ÷ HBM 池容量**。为什么不是「投递量 >= 池容量 - 自身存活」?
    因为 vLLM 的淘汰是 **LRU-by-touch**(命中即 touch -> 把块移到空闲队列尾部),每轮压力
    轮都被复用的会话前缀会被反复推回队尾。要让这样的块真正被淘汰,累计分配量必须绕空闲
    队列多圈。观测:1.5x 池时 canary 仍 89% 驻留 HBM;6x 池时 canary 全 rank `hbm==0`。
    故需求剂量取 `HBM_TOKENS_PER_RANK * EVICT_DOSE_X`(默认 4x,保守但可判真伪)。
    """
    dose: dict = {}
    for d in deliv:
        dose[d["rank"]] = dose.get(d["rank"], 0) + d["tokens"]
    own_live: dict = {}
    for sess in sessions:
        r = sess["rank"]
        if r is None:
            continue
        own_live[r] = own_live.get(r, 0) + args_lt
    need = HBM_TOKENS_PER_RANK * EVICT_DOSE_X
    rows = []
    worst = math.inf
    for r in sorted(set(dose) | set(own_live)):
        dm = dose.get(r, 0)
        margin = dm / need if need > 0 else math.inf
        worst = min(worst, margin)
        rows.append({"rank": r, "dose_tokens": dm, "own_live_tokens": own_live.get(r, 0),
                     "pool_tokens": HBM_TOKENS_PER_RANK,
                     "dose_x_pool": round(dm / HBM_TOKENS_PER_RANK, 3),
                     "need_tokens": need, "need_x_pool": EVICT_DOSE_X,
                     "margin": None if math.isinf(margin) else round(margin, 3)})
    return {"rows": rows, "evict_dose_x": EVICT_DOSE_X,
            "worst_margin": None if math.isinf(worst) else round(worst, 3)}


def find_helper_sids(args, need_by_rank, rec: Recorder) -> dict:
    """为剂量不足的 rank 找额外 sid(通过小请求探测落点),使每 rank 剂量可控。

    sid->rank 由 consistent_hash 唯一决定,但每个 run 的 sid 前缀不同 -> 分布可能偏斜
    (曾出现 4 会话全落 rank0)。若只按会话自己的 sid 施压,偏斜会让某些 rank 剂量
    不足、HBM 翻不动 -> V1 假失败。这里为欠账的 rank 补齐 sid。
    """
    got = {r: [] for r in need_by_rank}
    need = dict(need_by_rank)
    tried = 0
    while any(len(got[r]) < need[r] for r in need) and tried < 240:
        tried += 1
        sid = f"helper-{args.run_id}-{args.arm}-{random.randrange(1 << 32):08x}"
        try:
            s, _ = do_chat(args, build_prompt(4096, sid), sid, phase="helper")
        except Exception as e:  # noqa: BLE001
            print(f"    [warn] helper 探测失败: {e}")
            continue
        r = s["rank"]
        if r in need and len(got[r]) < need[r]:
            got[r].append(sid)
            rec.add(phase="helper_sid", arm=args.arm, rank=r, sid=sid)
    for r in sorted(need):
        print(f"    rank {r}: 需要 {need[r]} 个 helper sid,探测 {tried} 次,得到 {len(got[r])}")
    return got


def find_pinned_sids(args, rank: int, n: int) -> list:
    """找出 n 个 consistent_hash 落在指定 rank 的 sid(把并发会话压到同一张卡)。

    落点由 sid 唯一决定,没有解析式 ⇒ 只能"试":每次用新 sid 发一个小请求,
    看它落在哪个 rank。探针必须 > load_tokens_threshold(2048),否则 FAWA 行不打印、
    拿不到 rank;这里用 4096 tok(与 helper 探测同口径)。
    探测到的 sid 会被**两臂共用**(在 main() 里算一次放进 args.pinned_sids),
    这样两臂 sid 相同 -> rank 相同 -> 配对消差。
    """
    sids, tried = [], 0
    while len(sids) < n and tried < 600:
        tried += 1
        sid = f"{args.run_id}-p{rank}-{random.randrange(1 << 32):08x}"
        try:
            s, _ = do_chat(args, build_prompt(4096, sid), sid, phase="pinprobe")
        except Exception as e:  # noqa: BLE001
            print(f"    [warn] pin 探测失败: {e}")
            continue
        if s["rank"] == rank:
            sids.append(sid)
    print(f"  固定落点 rank={rank}: 需要 {n} 个 sid,探测 {tried} 次,得到 {len(sids)}")
    return sids


def pick_probe_sessions(sessions, per_rank: int) -> list:
    """抽查用的会话子集:每个 rank 取前 per_rank 个(0 = 全部)。

    canary / 落盘探针要证明的是**这个 rank 的 HBM 被挤干净了**,这是 rank 级属性,
    不是会话级属性(consistent_hash 下同 rank 的会话共用同一个 KV pool)。单卡轮里
    6 个会话都在同一张卡上,逐个重放 6 次 64k 只是把同一结论重复 6 遍,空等 30s。
    """
    if per_rank <= 0:
        return sessions
    seen: dict = {}
    out = []
    for s in sessions:
        r = s["rank"]
        if seen.get(r, 0) < per_rank:
            seen[r] = seen.get(r, 0) + 1
            out.append(s)
    return out


def pmap(items, fn, workers):
    """并发(workers>1)或串行执行 fn(item);单条失败只告警,不影响其它。"""
    def run_one(it):
        try:
            return fn(it)
        except Exception as e:  # noqa: BLE001
            print(f"    [warn] 子任务失败: {e}")
            return None
    if workers > 1 and len(items) > 1:
        with ThreadPoolExecutor(max_workers=workers) as ex:
            return [fut.result() for fut in as_completed([ex.submit(run_one, it) for it in items])]
    return [run_one(it) for it in items]


def persist_probe(args, sessions, turn, rec: Recorder, workers: int = 1) -> dict:
    """压力前确认"turn-1 的 KV 至少在外部缓存里"(区别于被 GC 吃掉 / 从未落盘)。

    用远离本会话的随机 sid 探一次,避免落到自己 rank 的 HBM 上;此时数据多半还在
    DDR(dump 刚完成),所以 `load>0` 只证明"外部缓存有",不证明"已在 SSD"——真正的
    SSD 证据是 OFF 臂被测样本在 DDR 被冲掉后的 `load` 值。

    **DP=1(单卡口径)必须跳过**:只有一张卡时"随机 sid"也一定落在被测卡上,探针会把
    「马上要被测的前缀」重新灌进 HBM(load=751/751);随后压力按 LRU 淘汰的是更旧的块,
    probe 刚写进去的块是最新的 ⇒ 被测样本 hbm>0,OFF 臂冷基准失效(曾观测到 TP8/DP1 下
    T2/T3 V1 全部不通过、OFF 臂样本 hbm_resident)。多卡口径下探针靠随机 sid
    落到别的卡、不会污染被测卡,故不跳过。冷基准改由被测样本自身的 `hbm==0 且 load>0`
    判定(V1),该探针只是"预先确认外部缓存有货"的便利检查。
    """
    if args.dp_size <= 1:
        print(f"\n  [落盘探针] turn {turn}: DP=1(单卡)跳过——探针与被测同卡,会把被测前缀"
              f"重新灌进 HBM,破坏 OFF 臂冷基准;冷基准改由被测样本 hbm==0 且 load>0 判定",
              flush=True)
        return {"rows": [], "ok": 0, "n": 0, "skipped": True}
    print(f"  [落盘探针] turn {turn}: 逐会话探测 turn{turn-1} 前缀是否可外部回源",
          flush=True)
    rows = []

    def one(sess):
        if (turn - 1) not in sess["turns"]:
            # 上一轮驱动没成功(例如 router 断路器瞬态 503)⇒ 无前缀可探,跳过而不是崩
            print(f"    [warn] probe s{sess['k']}: 缺 turn{turn-1} 样本(该轮驱动未成功),跳过")
            return
        tgt = sess["turns"][turn - 1]
        sid = f"probe-{args.run_id}-{args.arm}-{random.randrange(1 << 32):08x}"
        try:
            s, f = do_chat(args, tgt["prompt"], sid, phase="probe")
        except Exception as e:  # noqa: BLE001
            print(f"    [warn] probe s{sess['k']}: {e}")
            return
        load = f.get("load") or 0
        row = {"k": sess["k"], "turn": turn, "sid": sid, "rank": s["rank"],
               "elapsed_s": round(s["elapsed_s"], 3),
               "target_blocks": len(tgt["blocks"]),
               "ok": load > 0, **fawa_fields(f)}
        rows.append(row)
        rec.add(phase="probe", arm=args.arm, **row)
        print(f"    s{sess['k']:<2} rank={row['rank']} {row['elapsed_s']:5.2f}s"
              f"  load={f.get('load')}/{row['target_blocks']} hbm={f.get('hbm')}"
              f"  [{'外部缓存有' if row['ok'] else '未命中->可能被GC吃/未落盘'}]", flush=True)

    pmap(sessions, one, workers)
    n_ok = sum(1 for r in rows if r["ok"])
    if rows and n_ok < len(rows):
        print(f"  [warn] 落盘探针 {n_ok}/{len(rows)} 命中;未命中样本在 OFF 臂会退化为"
              f" prefill(flag=no_external_load),届时该样本已从 V2 剔除")
    return {"rows": rows, "ok": n_ok, "n": len(rows)}


def resolve_pressure_entries(args, pool) -> int:
    """确定压力素材条数 `--pressure-entries`(0=全部,-1=自动),并校验池够不够。

    压力步要把目标前缀挤出 DDR 的 FA 缓存,素材体量须 >= FA 容量 × 余量,故默认取
    池的前 N 条(N = FA容量×余量 / 单条体量)。N 随 `--cache-buffer-capacity-gb`
    (或环境变量 UCM_CACHE_BUFFER_CAPACITY_GB)自适应。返回 0 表示可用,非 0 需重放池。
    """
    n_all = len(pool["entries"])
    gb = statistics.median(e["bytes"] for e in pool["entries"])
    if args.pressure_entries < 0:
        args.pressure_entries = entries_for_fa(args, pool)
        src = "自动(DDR 容量推导)"
    else:
        src = "显式"
    want = args.pressure_entries
    print(f"\n   压力素材: 池共 {n_all} 条×{gb / (1 << 30):.2f}GiB;用前 {want or n_all} 条"
          f"(来源 {src});需 >= {ddr_need_bytes(args) / (1 << 30):.1f}GiB"
          f"(FA {args.fa_gib:g}GiB ×{args.ddr_margin:g})")
    if want and want * gb < ddr_need_bytes(args):
        print(f"   [warn] 前 {want} 条仅 {want * gb / (1 << 30):.1f}GiB < 需要"
              f" {ddr_need_bytes(args) / (1 << 30):.1f}GiB,可能挤不动 DDR")
    if want > n_all:
        print(f"[FAIL] 需要前 {want} 条,池只有 {n_all} 条。请按当前 DDR 重新预放:\n"
              f"       python3 verify_prefetch_effect_fast.py --prepare-pool "
              f"--pool-file {args.pool_file}")
        return 1
    return 0


def pressure_step(args, pool, sids_by_rank, sessions, turn, rec: Recorder) -> dict:
    entries = pool["entries"]
    if args.pressure_entries > 0:
        entries = entries[: args.pressure_entries]
    n = len(entries)
    roster = {r: len(v) for r, v in sorted(sids_by_rank.items())}
    jobs = args.pressure_passes * sum(roster.values())
    per_rank_in_flight = min(roster.values()) if roster else 0
    need_conc = math.ceil(HBM_TOKENS_PER_RANK / args.flush_tokens) + 1
    print(f"\n  [压力] turn {turn}: {args.pressure_passes} 趟 × {roster} 条"
          f" = {jobs} 条(每趟每 rank 同时起 {per_rank_in_flight} 条)", flush=True)
    print(f"  [压力] HBM 挤占核算: 每 rank 在飞 {per_rank_in_flight} × {args.flush_tokens} tok"
          f" = {per_rank_in_flight * args.flush_tokens / 1e6:.2f}M tok"
          f" vs HBM 池 {HBM_TOKENS_PER_RANK/1e6:.2f}M tok -> "
          + ("超池,新分配必须复用缓存块" if per_rank_in_flight * args.flush_tokens
             > HBM_TOKENS_PER_RANK
             else f"未超池,只消耗空闲块、不动缓存块(建议每 rank 在飞 >= {need_conc})"),
          flush=True)
    t0 = time.perf_counter()

    def one(job):
        p, rank, sid, e = job
        s, f = do_chat(args, build_prompt(args.flush_tokens, e["nonce"]), sid,
                       phase="pressure")
        if len(s["block_ids"]) != e["blocks"]:
            print(f"    [warn] 压力 {e['nonce'][-9:]} blocks {len(s['block_ids'])}"
                  f" != 预放 {e['blocks']}")
        return {"pass": p, "intended_rank": rank, "sid": sid, "entry": e["nonce"],
                "rank": s["rank"], "tokens": e["prompt_tokens"],
                "elapsed_s": round(s["elapsed_s"], 3), **fawa_fields(f)}

    deliv = []
    with PoolMonitor(args.metrics_url) as mon:
        for p in range(1, args.pressure_passes + 1):
            batch = []
            for rank in sorted(sids_by_rank):
                sids = sids_by_rank[rank]
                for i, sid in enumerate(sids):
                    batch.append((p, rank, sid, entries[(p * len(sids) + i + rank) % n]))
            with ThreadPoolExecutor(
                    max_workers=max(1, args.pressure_concurrency or len(batch))) as ex:
                futs = [ex.submit(one, j) for j in batch]
                for fut in as_completed(futs):
                    try:
                        d = fut.result()
                    except Exception as e:  # noqa: BLE001
                        print(f"    [warn] 压力请求失败: {e}")
                        continue
                    deliv.append(d)
                    rec.add(phase="pressure", arm=args.arm, turn=turn, **d)
                    drift = "" if d["rank"] == d["intended_rank"] else " [落点漂移!]"
                    print(f"    P{d['pass']} r{d['intended_rank']} {d['entry'][-9:]}:"
                          f" {d['elapsed_s']:6.2f}s rank={d['rank']} hbm={d['hbm']}"
                          f" load={d['load']} dump={d['dump']}{drift}", flush=True)
    wall = time.perf_counter() - t0
    peak = mon.peak
    dose = pressure_dose(args.turn_lens[turn], sessions, deliv)
    print(f"  [压力] 完成 {len(deliv)}/{jobs} 条, wall {wall:.1f}s")
    for r in sorted(peak):
        p = peak[r]
        print(f"         rank {r}: HBM 池峰值占用 {p.get('usage', 0.0)*100:.0f}%"
              f" / 在飞 {p.get('running', 0.0):.0f} 条"
              f" / capacity 等待 {p.get('wait_capacity', 0.0):.0f} 条"
              f" / deferred 等待 {p.get('wait_deferred', 0.0):.0f} 条")
    for row in dose["rows"]:
        print(f"         rank {row['rank']}: 投递 {row['dose_tokens']/1e6:.2f}M tok"
              f" = {row['dose_x_pool']}x 池"
              f" / 自身存活 {row['own_live_tokens']/1024:.0f}k tok"
              f" / 需求 {row['need_tokens']/1e6:.2f}M tok({row['need_x_pool']}x 池)"
              f" / 达成率 {row['margin']}x")
    if dose["worst_margin"] is not None and dose["worst_margin"] < 1.0:
        print(f"  [warn] HBM 剂量不足(最差 {dose['worst_margin']}x < 1.0x,即累计投递未达"
              f" {EVICT_DOSE_X}x 池),canary 可能仍驻留 HBM;"
              f"考虑提高 --pressure-per-rank 或 --pressure-passes")
    dose["deliveries"] = len(deliv)
    dose["wall_s"] = round(wall, 1)
    dose["pool_peak"] = {r: {k: round(v, 3) for k, v in p.items()} for r, p in sorted(peak.items())}
    dose["hbm_saturated_ranks"] = sorted(
        r for r, p in peak.items() if p.get("usage", 0.0) >= 0.95)
    return dose


# ===== canary =====

def canary_check(args, sessions, turn, rec: Recorder, workers: int = 1) -> dict:
    print(f"\n  [canary] turn {turn}: 逐会话重放**专用** canary 前缀,期望 hbm==0(压力已把它挤出 HBM)",
          flush=True)
    rows = []

    def one(sess):
        # 用**专用** canary 前缀(与 turn1 不同):canary 在测量之前跑,若重放被测前缀
        # 会把命中块重新灌回 DDR,抹平 OFF 臂的冷启动(见 arm_run.drive_canary_seed)。
        t1 = sess.get("canary") or sess["turns"][1]
        try:
            s, f = do_chat(args, t1["prompt"], sess["sid"], phase="canary")
        except Exception as e:  # noqa: BLE001
            print(f"    [warn] canary s{sess['k']}: {e}")
            return
        hbm = f.get("hbm")
        ratio = (hbm / t1["prompt_tokens"]) if (hbm is not None and t1["prompt_tokens"]) else None
        row = {"k": sess["k"], "sid": sess["sid"], "rank": s["rank"],
               "elapsed_s": round(s["elapsed_s"], 3), "prompt_tokens": t1["prompt_tokens"],
               "hbm_ratio": None if ratio is None else round(ratio, 4), **fawa_fields(f)}
        rows.append(row)
        rec.add(phase="canary", arm=args.arm, turn=turn, **row)
        print(f"    s{row['k']:<2} rank={row['rank']} {row['elapsed_s']:5.2f}s"
              f"  hbm={hbm} ({'-' if ratio is None else f'{ratio*100:.0f}%'})"
              f"  load={f.get('load')}  [{'已挤出' if hbm == 0 else '仍驻留'}]", flush=True)

    pmap(sessions, one, workers)
    by_rank: dict = {}
    for r in rows:
        by_rank.setdefault(r["rank"], []).append(r)
    zero_ranks = [r for r, v in by_rank.items() if all((x["hbm"] or 0) == 0 for x in v)]
    low_ranks = [r for r, v in by_rank.items()
                 if all((x["hbm_ratio"] or 0) <= 0.2 for x in v)]
    print(f"  [canary] 活动 rank={sorted(by_rank)}  hbm==0 的 rank={sorted(zero_ranks)}"
          f"  hbm_ratio<=20% 的 rank={sorted(low_ranks)}")
    return {"rows": rows, "active_ranks": sorted(by_rank),
            "hbm_zero_ranks": sorted(zero_ranks), "hbm_low_ranks": sorted(low_ranks)}


# ===== 单轮执行 =====

def prefetch_round(args, sessions, turn, rec: Recorder) -> dict:
    """ON 臂:在"工具间隙"开始前逐会话预取 turn-1 的前缀(seed 与测量请求同 sid=同落点)。

    注意 `wall` 是 router 的受理延迟(预取在服务端异步落 DDR),真实收益要看随后的
    TTFT 与 metrics ddr_ratio;这里先记 wall,再留出工具间隙让预取落地。
    """
    out = {}
    print(f"  [预取] turn {turn}: 逐会话预取 turn{turn-1} 前缀", flush=True)
    for sess in sessions:
        if (turn - 1) not in sess["turns"]:
            # 上一轮缺失 ⇒ 没有可预热的前缀;跳过(该会话本轮样本会带 no_target 标记)
            print(f"    [warn] prefetch s{sess['k']}: 缺 turn{turn-1} 样本,跳过")
            continue
        tgt = sess["turns"][turn - 1]
        r = prefetch_warm(args.vllm_url, tgt["blocks"], args.warm_chunk, args.timeout,
                          headers={"x-session-id": sess["sid"]})
        info = {"target_blocks": len(tgt["blocks"]),
                "prefetch_count": r["prefetch_count"], "modes": r["modes"],
                "dp_ranks": r["dp_ranks"], "wall_s": round(r["elapsed_s"], 3)}
        out[sess["k"]] = info
        rec.add(phase="prefetch", arm=args.arm, k=sess["k"], turn=turn,
                sid=sess["sid"], **info)
        print(f"    s{sess['k']:<2} {info['target_blocks']} blocks -> "
              f"prefetch_count={info['prefetch_count']} dp_rank={info['dp_ranks']}"
              f"  wall={info['wall_s']:.2f}s", flush=True)
    return out


def run_turn(args, sess, turn, arm, rec: Recorder, prefetch=None) -> dict:
    """跑一轮(被测轮带 metrics 差分)。"""
    L = args.turn_lens[turn]
    prompt = build_prompt(L, sess["nonce"])
    snap_b = delta = None
    if turn in args.measure_turns:
        snap_b = metrics_snapshot(args.metrics_url)
        time.sleep(0.4)
    s, f = do_chat(args, prompt, sess["sid"], phase="turn")
    if turn in args.measure_turns:
        time.sleep(0.4)
        delta = metrics_delta(snap_b, metrics_snapshot(args.metrics_url))
    rank_delta = (delta or {}).get(s["rank"]) if s["rank"] is not None else None

    rec_row = {
        "phase": "measure" if turn in args.measure_turns else "advance",
        "arm": arm, "k": sess["k"], "turn": turn, "sid": sess["sid"],
        "prompt_tokens": s["prompt_tokens"], "blocks": len(s["block_ids"]),
        "elapsed_s": round(s["elapsed_s"], 3), "rank": s["rank"],
        "prefetch": prefetch, **fawa_fields(f),
        "metrics": None if rank_delta is None else {
            "rank": s["rank"],
            "d_ddr": round(rank_delta["d_cache_load_shards_total"], 1),
            "d_wait": round(rank_delta["d_cache_load_wait_shards_total"], 1),
            "ddr_ratio": rank_delta["ddr_ratio"], "ssd_ratio": rank_delta["ssd_ratio"],
        },
    }
    if turn in args.measure_turns:
        prev = sess["turns"].get(turn - 1)
        # 上一轮缺失(该轮驱动未成功)⇒ 无法判定"整段前缀是否全部来自缓存",记无效样本而非崩
        tgt_blocks = None if prev is None else len(prev["blocks"])
        tgt_tokens = 0 if tgt_blocks is None else tgt_blocks * UCM_TOKENS_PER_BLOCK
        load = f.get("load")
        hbm = f.get("hbm") or 0
        # 有效性只看"整段前缀是否都从缓存拿到"(hbm tokens + load blocks 换算后覆盖 target),
        # SSD 与 DDR 的区分交给 metrics 差分;partial 仅在 hbm==0 时才有意义。
        covered = hbm + (load or 0) * UCM_TOKENS_PER_BLOCK
        flags = []
        if prev is None:
            flags.append("no_target")
        elif (load or 0) == 0 and hbm < 0.9 * tgt_tokens:
            flags.append("no_external_load")
        elif hbm == 0 and covered < 0.9 * tgt_tokens:
            flags.append("partial_load")
        if hbm > 0:
            flags.append("hbm_resident")
        if s["rank"] != sess["rank"]:
            flags.append("rank_drift")
        rec_row.update({"target_blocks": tgt_blocks, "flags": flags,
                        "valid_cold": bool(prev is not None and hbm == 0
                                           and covered >= 0.9 * tgt_tokens),
                        "concurrent": bool(args.measure_concurrency != 1)})
    rec.add(**rec_row)
    if turn in args.measure_turns:
        m = rec_row["metrics"] or {}
        print(f"    turn{turn} [{arm}] {s['elapsed_s']:6.2f}s rank={s['rank']}"
              f" hbm={f.get('hbm')} ext={f.get('external')} load={f.get('load')}"
              f"->{rec_row['target_blocks']} dump={f.get('dump')}"
              f" | ddr={_pct(m.get('ddr_ratio'))} ssd={_pct(m.get('ssd_ratio'))}"
              f" {('[' + ','.join(rec_row['flags']) + ']') if rec_row['flags'] else ''}",
              flush=True)
    else:
        print(f"    turn{turn} [{arm}] {s['elapsed_s']:6.2f}s rank={s['rank']}"
              f" hbm={f.get('hbm')} load={f.get('load')}", flush=True)
    sess["turns"][turn] = {"blocks": s["block_ids"], "prompt": prompt,
                           "prompt_tokens": s["prompt_tokens"], "rank": s["rank"]}
    return rec_row


def _pct(v):
    return "-" if v is None else f"{v*100:.0f}%"


# ===== 单臂 =====

def arm_run(args, arm, pool, rec: Recorder) -> dict:
    args.arm = arm
    div = args.scan_max or args.pressure_passes
    print(f"\n{'='*72}\n== 臂 [{arm}]{'（不预取）' if arm == 'off' else '（预取）'}"
          f"  sessions={args.sessions} turns={args.turns}"
          f" measure={sorted(args.measure_turns)} pressure={sorted(args.pressure_turns)}"
          f"\n{'='*72}", flush=True)
    if not args.no_drop_caches:
        print(f"  清 page cache: {'成功' if drop_caches() else '失败'}")
    sessions = []
    for k in range(args.sessions):
        # sid 两臂**相同** -> consistent_hash 落点相同(同 rank),配对才能消差;
        # nonce 两臂**不同** -> KV 内容不同,不会撞上一臂残留在 HBM/DDR 里的前缀。
        # --pin-rank >=0 时用 main() 预探好的 pinned_sids(全部落同一张卡)。
        sid = args.pinned_sids[k] if args.pinned_sids else f"{args.run_id}-s{k}"
        nonce = f"{args.run_id}-{arm}-s{k}"
        sessions.append({"k": k, "sid": sid, "nonce": nonce, "rank": None, "turns": {}})

    def drive(fn, items, workers, label):
        """并发(workers>1)或串行驱动 items;单个失败只告警,不作废整臂。"""
        def run_one(it):
            try:
                fn(it)
            except Exception as e:  # noqa: BLE001
                print(f"    [warn] {label} 失败: {e}")
        if workers > 1 and len(items) > 1:
            with ThreadPoolExecutor(max_workers=workers) as ex:
                futs = [ex.submit(run_one, it) for it in items]
                for fut in as_completed(futs):
                    fut.result()
        else:
            for it in items:
                run_one(it)

    def drive_turn1(sess):
        """第 1 轮:预填前缀(无前序可复用)并记下 rank。"""
        prompt = build_prompt(args.turn_lens[1], sess["nonce"])
        s, f = do_chat(args, prompt, sess["sid"], phase="turn1")
        sess["rank"] = s["rank"]
        sess["turns"][1] = {"blocks": s["block_ids"], "prompt": prompt,
                            "prompt_tokens": s["prompt_tokens"], "rank": s["rank"]}
        rec.add(phase="advance", arm=arm, k=sess["k"], turn=1, sid=sess["sid"],
                prompt_tokens=s["prompt_tokens"], blocks=len(s["block_ids"]),
                elapsed_s=round(s["elapsed_s"], 3), rank=s["rank"],
                prefetch=None, **fawa_fields(f))
        print(f"[turn1] s{sess['k']:<2} {s['elapsed_s']:6.2f}s rank={s['rank']}"
              f" {s['prompt_tokens']} tok / {len(s['block_ids'])} blocks"
              f"  hbm={f.get('hbm')} load={f.get('load')} dump={f.get('dump')}",
              flush=True)

    def drive_canary_seed(sess):
        """给每个会话造一段**专用**canary 前缀(与后面的被测前缀无关)。

        为什么不直接拿 turn1 当 canary:canary 在压力之后、**测量之前**重放,会把
        命中块重新灌进 DDR,被测样本的 ddr_ratio 被抬高(如 501/751=67%)—— OFF 臂
        不再冷,收益被抹平。
        专用前缀内容不同 => 灌进来的块与被测前缀无交集,canary 只证明"这个 rank 的
        HBM 被压力翻过了",不污染被测样本。
        """
        nonce = f"{args.run_id}-{arm}-s{sess['k']}-canary"
        prompt = build_prompt(args.canary_tokens, nonce)
        s, f = do_chat(args, prompt, sess["sid"], phase="canary_seed")
        sess["canary"] = {"blocks": s["block_ids"], "prompt": prompt,
                          "prompt_tokens": s["prompt_tokens"], "rank": s["rank"]}
        rec.add(phase="canary_seed", arm=arm, k=sess["k"], sid=sess["sid"],
                prompt_tokens=s["prompt_tokens"], blocks=len(s["block_ids"]),
                elapsed_s=round(s["elapsed_s"], 3), rank=s["rank"], **fawa_fields(f))
        print(f"[canary_seed] s{sess['k']:<2} {s['elapsed_s']:6.2f}s rank={s['rank']}"
              f" {s['prompt_tokens']} tok / {len(s['block_ids'])} blocks"
              f"  hbm={f.get('hbm')} dump={f.get('dump')}", flush=True)

    # 非被测轮(advance)默认也**并发**:真实 agent 车队里多会话就是同时在跑的,且
    # 聚合存活 KV(6 x 96k = 0.58M tok)远小于 HBM 池(2.08M),不会互相挤掉;
    # 串行只是空等。--advance-concurrency 1 = 全集群版的串行热身。
    adv_conc = args.advance_concurrency or args.sessions
    print(f"  [turn1] {len(sessions)} 会话,并发={adv_conc}", flush=True)
    drive(drive_turn1, sessions, adv_conc, "turn1")
    drive(drive_canary_seed, sessions, adv_conc, "canary_seed")
    if any(s["rank"] is None for s in sessions):
        bad = [s["k"] for s in sessions if s["rank"] is None]
        print(f"  [warn] {len(bad)} 个会话 turn1 未成功(k={bad}),已剔除")
        sessions = [s for s in sessions if s["rank"] is not None]
    rank_load = {}
    rank_map = {}
    for sess in sessions:
        rank_load[sess["rank"]] = rank_load.get(sess["rank"], 0) + 1
        rank_map["s" + str(sess["k"])] = sess["rank"]
    print(f"  sid->rank 映射: {json.dumps(rank_map, ensure_ascii=False)}")
    print(f"  每 rank 会话数: {json.dumps(rank_load, ensure_ascii=False)}")

    # 压力 roster:每 rank 每趟投递 max(ceil(per_rank/div), 本 rank 会话数) 条,
    # 欠账的 rank 用 helper sid 补齐(见 find_helper_sids)。
    # div 在扫描模式下固定为 max(scan),使剂量随趟数单调增长(V4 判据)。
    per_pass = max(1, math.ceil(args.pressure_per_rank / max(1, div)))
    target = {r: max(per_pass, n) for r, n in rank_load.items()}
    need_helpers = {r: target[r] - rank_load[r] for r in rank_load if target[r] > rank_load[r]}
    helper = {}
    if need_helpers:
        print(f"  helper sid 探测(补齐各 rank 剂量): {json.dumps(need_helpers, ensure_ascii=False)}")
        helper = find_helper_sids(args, need_helpers, rec)
    sids_by_rank = {r: [s["sid"] for s in sessions if s["rank"] == r] + helper.get(r, [])
                    for r in rank_load}
    dose_plan = {r: len(v) * args.pressure_passes * args.flush_tokens for r, v in sids_by_rank.items()}
    print(f"  压力 roster(每 rank sid 数): {json.dumps({r: len(v) for r, v in sids_by_rank.items()})}"
          f"  -> 计划剂量/rank: {json.dumps({r: round(v/1e6, 2) for r, v in dose_plan.items()})} M tok"
          f" = {json.dumps({r: round(v/HBM_TOKENS_PER_RANK, 2) for r, v in dose_plan.items()})} x 池"
          f"(需求 {EVICT_DOSE_X}x)")

    print(f"  等 --dump-settle {args.dump_settle:.0f}s 让 turn1 落盘...")
    time.sleep(args.dump_settle)

    probe_sess = pick_probe_sessions(sessions, args.probe_per_rank)
    if len(probe_sess) < len(sessions):
        print(f"  canary/落盘探针抽查:{len(probe_sess)}/{len(sessions)} 个会话"
              f"(每 rank {args.probe_per_rank} 个,rank 级属性不必逐会话重复)")
    canary = None
    arm_info = {"arm": arm, "rank_load": rank_load, "rank_map": rank_map,
                "sids_by_rank": {r: len(v) for r, v in sids_by_rank.items()},
                "dose_plan_mtok": {r: round(v / 1e6, 2) for r, v in dose_plan.items()},
                "doses": [], "canary": None}
    for turn in range(2, args.turns + 1):
        print(f"\n-- turn {turn} (L={args.turn_lens[turn]} tok)"
              f"{'  [被测]' if turn in args.measure_turns else ''}", flush=True)
        if turn in args.pressure_turns:
            persist_probe(args, probe_sess, turn, rec, workers=adv_conc)
            dose = pressure_step(args, pool, sids_by_rank, sessions, turn, rec)
            arm_info["doses"].append({"turn": turn, **dose})
            print(f"  等 --settle-after-pressure {args.settle_after_pressure:.0f}s...")
            time.sleep(args.settle_after_pressure)
            if args.hbm_canary and canary is None:
                canary = canary_check(args, probe_sess, turn, rec, workers=adv_conc)
                arm_info["canary"] = canary
                time.sleep(args.settle_after_pressure)
        prefetch_info = {}
        if arm == "on" and turn in args.measure_turns:
            prefetch_info = prefetch_round(args, sessions, turn, rec)
        print(f"  等 --tool-gap {args.tool_gap:.0f}s(工具间隙;也是预取落地窗口)")
        time.sleep(args.tool_gap)
        # 被测轮:真实 agent 服务上是多会话同时在跑(这里把并发作为默认)。
        conc = args.measure_concurrency
        workers = (len(sessions) if conc <= 0 else conc) if turn in args.measure_turns else adv_conc

        def drive_this(sess):
            try:
                run_turn(args, sess, turn, arm, rec, prefetch=prefetch_info.get(sess["k"]))
            except Exception as e:  # noqa: BLE001
                print(f"    [warn] s{sess['k']} turn{turn} 失败: {e}")
                rec.add(phase="error", arm=arm, k=sess["k"], turn=turn,
                        sid=sess["sid"], error=str(e))

        drive(drive_this, sessions, workers, f"turn{turn}")
    arm_info["sessions"] = [{"k": s["k"], "sid": s["sid"], "rank": s["rank"]} for s in sessions]
    return arm_info


# ===== 汇总 =====

def summarize(args, arms_info, records, v4) -> dict:
    measures = [r for r in records if r.get("phase") == "measure"]
    out = {"paired": [], "by_arm": {}, "by_arm_turn": {}}

    for arm in [a["arm"] for a in arms_info]:
        rows = [r for r in measures if r["arm"] == arm]
        out["by_arm"][arm] = {
            "n": len(rows),
            "median_ttft_s": med([r["elapsed_s"] for r in rows]),
            "hbm_zero_rate": (sum(1 for r in rows if (r.get("hbm") or 0) == 0) / len(rows))
            if rows else None,
            "cold_valid_rate": (sum(1 for r in rows if r.get("valid_cold")) / len(rows))
            if rows else None,
            "ddr_hist": _hist([r["metrics"]["ddr_ratio"] for r in rows
                               if r.get("metrics") and r["metrics"]["ddr_ratio"] is not None]),
            "ssd_med": med([r["metrics"]["ssd_ratio"] for r in rows
                            if r.get("metrics") and r["metrics"]["ssd_ratio"] is not None]),
            "ddr_ge80_rate": _rate([r["metrics"]["ddr_ratio"] for r in rows
                                    if r.get("metrics") and r["metrics"]["ddr_ratio"] is not None],
                                   0.8),
            "ssd_ge80_rate": _rate([r["metrics"]["ssd_ratio"] for r in rows
                                    if r.get("metrics") and r["metrics"]["ssd_ratio"] is not None],
                                   0.8),
            "flag_counts": _count_flags(rows),
            "prefetch_wall_mean": (
                statistics.mean([r["prefetch"]["wall_s"] for r in rows if r.get("prefetch")])
                if any(r.get("prefetch") for r in rows) else None),
            "prefetch_wall_max": max([r["prefetch"]["wall_s"] for r in rows
                                      if r.get("prefetch")], default=None),
        }
        turns = {}
        for turn in sorted({r["turn"] for r in rows}):
            g = [r for r in rows if r["turn"] == turn]
            turns[turn] = {
                "n": len(g), "prompt_tokens": med([r["prompt_tokens"] for r in g]),
                "median_ttft_s": med([r["elapsed_s"] for r in g]),
                "load_med": med([r["load"] for r in g if r.get("load") is not None]),
                "target_blocks_med": med([r["target_blocks"] for r in g
                                          if r.get("target_blocks") is not None]),
                "hbm_zero_rate": (sum(1 for r in g if (r.get("hbm") or 0) == 0) / len(g)),
                # 整批口径(与请求次序无关):墙钟 = 最早发出 → 最后答完;
                # ΣTTFT = 这一批请求各自耗时的总和(= n × 平均耗时,机器的总请求时间)
                "fleet_wall_s": _fleet_wall(g),
                "sum_ttft_s": round(sum(r["elapsed_s"] for r in g), 3),
            }
        out["by_arm_turn"][arm] = turns

    off = {(r["k"], r["turn"]): r for r in measures if r["arm"] == "off"}
    on = {(r["k"], r["turn"]): r for r in measures if r["arm"] == "on"}
    for key in sorted(set(off) & set(on)):
        a, b = off[key], on[key]
        if a.get("flags") and any(f in ("no_external_load", "rank_drift") for f in a["flags"]):
            continue
        if b.get("flags") and any(f in ("no_external_load", "rank_drift") for f in b["flags"]):
            continue
        if not a.get("valid_cold"):
            continue
        d = a["elapsed_s"] - b["elapsed_s"]
        out["paired"].append({"k": key[0], "turn": key[1],
                              "off_s": a["elapsed_s"], "on_s": b["elapsed_s"],
                              "saved_s": round(d, 3),
                              "speedup": round(a["elapsed_s"] / b["elapsed_s"], 3)
                              if b["elapsed_s"] else None,
                              "off_ssd_ratio": (a.get("metrics") or {}).get("ssd_ratio"),
                              "on_ddr_ratio": (b.get("metrics") or {}).get("ddr_ratio"),
                              "off_hbm": a.get("hbm"), "on_hbm": b.get("hbm")})
    diffs = [p["saved_s"] for p in out["paired"]]
    speeds = [p["speedup"] for p in out["paired"] if p["speedup"]]
    lo, hi = bootstrap_median_ci(diffs)
    by_turn = {}
    for t in sorted({p["turn"] for p in out["paired"]}):
        g = [x for x in out["paired"] if x["turn"] == t]
        by_turn[t] = {"n": len(g), "median_saved_s": med([x["saved_s"] for x in g]),
                      "median_speedup": med([x["speedup"] for x in g if x["speedup"]])}
    out["paired_stats"] = {
        "n": len(diffs), "median_saved_s": med(diffs), "median_speedup": med(speeds),
        "ci_low": lo, "ci_high": hi, "by_turn": by_turn,
    }

    # ---- 次序配对(本快速版的**主口径**) ----
    # 6 个会话是**同时**发出的,引擎在同一张卡上串行处理它们,谁先谁后由引擎调度决定,
    # 两臂之间不可控。按 (session, turn) 配对 => 把"排队位次"的差异当成了收益:观测到
    # 假负样本、配对中位偏负、bootstrap CI 下界为负,而同一批数据的整批口径为正。
    # 与位次无关的口径是「**第 i 个答完的请求**」:两臂各按完成时刻(ts)排序后逐位配对,
    # 既保留配对消差、又不受位次置换影响
    # (如"最先答完的那个从 6.1s 提前到 4.9s,最后答完的从 25.5s 提前到 21.2s")。
    def _ordered(arm):
        rows = [r for r in measures if r["arm"] == arm and r.get("valid_cold")
                and not any(f in ("no_external_load", "rank_drift")
                            for f in (r.get("flags") or []))]
        return sorted(rows, key=lambda r: (r["turn"], r["ts"]))

    oo, nn = _ordered("off"), _ordered("on")
    order_pairs = []
    for turn in sorted({r["turn"] for r in oo} & {r["turn"] for r in nn}):
        ga = [r for r in oo if r["turn"] == turn]
        gb = [r for r in nn if r["turn"] == turn]
        for i, (a, b) in enumerate(zip(ga, gb)):
            order_pairs.append({
                "turn": turn, "order": i + 1, "off_k": a["k"], "on_k": b["k"],
                "off_s": a["elapsed_s"], "on_s": b["elapsed_s"],
                "saved_s": round(a["elapsed_s"] - b["elapsed_s"], 3),
                "speedup": (round(a["elapsed_s"] / b["elapsed_s"], 3)
                            if b["elapsed_s"] else None)})
    o_diffs = [p["saved_s"] for p in order_pairs]
    o_speeds = [p["speedup"] for p in order_pairs if p["speedup"]]
    o_lo, o_hi = bootstrap_median_ci(o_diffs)
    out["paired_by_order"] = order_pairs
    out["order_stats"] = {
        "n": len(o_diffs), "median_saved_s": med(o_diffs), "median_speedup": med(o_speeds),
        "mean_saved_s": (round(statistics.mean(o_diffs), 3) if o_diffs else None),
        "ci_low": o_lo, "ci_high": o_hi,
        "note": "两臂各自按完成时刻排序后逐位配对;次序无关,是单卡串行队列下的正确口径",
    }

    # ---- 整批口径(与次序无关,作为 V2 的加固项) ----
    agg = {}
    for turn in sorted(set(out["by_arm_turn"].get("off", {}))
                       & set(out["by_arm_turn"].get("on", {}))):
        o = out["by_arm_turn"]["off"][turn]
        n_ = out["by_arm_turn"]["on"][turn]
        agg[turn] = {
            "n": o.get("n"), "prompt_tokens": o.get("prompt_tokens"),
            "off_median_ttft_s": o.get("median_ttft_s"),
            "on_median_ttft_s": n_.get("median_ttft_s"),
            "median_ttft_saved_s": (
                round(o["median_ttft_s"] - n_["median_ttft_s"], 3)
                if o.get("median_ttft_s") is not None
                and n_.get("median_ttft_s") is not None else None),
            "off_fleet_wall_s": o.get("fleet_wall_s"),
            "on_fleet_wall_s": n_.get("fleet_wall_s"),
            "off_sum_ttft_s": o.get("sum_ttft_s"),
            "on_sum_ttft_s": n_.get("sum_ttft_s"),
            "median_ttft_speedup": _ratio(o.get("median_ttft_s"), n_.get("median_ttft_s")),
            "fleet_wall_speedup": _ratio(o.get("fleet_wall_s"), n_.get("fleet_wall_s")),
            "sum_ttft_speedup": _ratio(o.get("sum_ttft_s"), n_.get("sum_ttft_s")),
        }
    out["aggregate"] = agg

    def _agg_all(key, thr):
        vals = [v[key] for v in agg.values() if v.get(key) is not None]
        return bool(vals) and all(v >= thr for v in vals)

    off_arm = out["by_arm"].get("off", {})
    on_arm = out["by_arm"].get("on", {})
    canary = next((a["canary"] for a in arms_info if a["arm"] == "off" and a.get("canary")), None)
    doses_ok = all((d.get("worst_margin") is None or d["worst_margin"] >= 1.0)
                   for a in arms_info for d in a.get("doses", []))
    ps = out["paired_stats"]
    # 并发测量时,/metrics 前后差分里混进了同批其它会话的 shards,ddr/ssd 之比不再
    # 归属单个请求;此时分层 metrics 仅作参考(不参与判定),前提证据改用
    # FAWA 的逐请求字段(hbm==0 且 load>0)。
    serial = all(not r.get("concurrent") for r in measures) if measures else False

    # 用户前提只要求"取数不在 HBM(DDR 或 SSD 皆可)",故 V1 的判据只到"非 HBM":
    #   - OFF 臂冷样本 `hbm==0 且 load>0` 的占比 >= 90%
    #   - canary 的 `hbm==0` **覆盖全部活动 rank**(单卡轮=那 1 张卡;多卡轮=全部)
    #   - HBM 剂量 >= EVICT_DOSE_X x 池(否则是"没挤动",不能下结论)
    # 分层比例(ddr/ssd)与 OFF 臂 ssd 占比仅作参考信息,不作为通过条件。
    active_ranks = set(canary["active_ranks"]) if canary else set()
    zero_ranks = set(canary["hbm_zero_ranks"]) if canary else set()
    v1 = {
        "cold_valid_rate_ge_90": (off_arm.get("cold_valid_rate") or 0) >= 0.9,
        "canary_all_active_ranks_hbm_zero": bool(active_ranks and zero_ranks == active_ranks),
        "dose_ge_evict_x_pool": doses_ok,
    }
    v1b = {"on_ddr_ratio_ge_80": (None if not serial
                                  else (on_arm.get("ddr_ge80_rate") or 0) >= 0.9)}
    # V2 用**次序配对**(主口径)+ **整批口径**(加固):两者都与"请求排队位次"无关。
    # 按 sid 的配对(ps)只作参考展示,不参与判定 —— 位次在两臂间不可控。
    os_ = out["order_stats"]
    v2 = {"order_median_saved_ge_0.10s": (os_["median_saved_s"] or 0) >= 0.10,
          "order_median_speedup_ge_1.15x": (os_["median_speedup"] or 0) >= 1.15,
          "order_ci_low_gt_0": (os_["ci_low"] is not None and os_["ci_low"] > 0),
          "fleet_wall_speedup_ge_1.10x": _agg_all("fleet_wall_speedup", 1.10),
          "sum_ttft_speedup_ge_1.10x": _agg_all("sum_ttft_speedup", 1.10)}
    v3 = {"prefetch_wall_mean_le_tool_gap": (
        (on_arm.get("prefetch_wall_mean") or 0) <= args.tool_gap)}
    verdict_v1 = all(v1.values())
    verdict_v2 = all(v2.values())
    verdict_v3 = all(v3.values())
    verdict_v1b = all(v for v in v1b.values() if v is not None)
    out["serial_attribution"] = serial
    out["refs"] = {
        "off_ssd_ge80_rate": off_arm.get("ssd_ge80_rate"),
        "off_ddr_ge80_rate": off_arm.get("ddr_ge80_rate"),
        "on_ddr_ge80_rate": on_arm.get("ddr_ge80_rate"),
        "on_ssd_ge80_rate": on_arm.get("ssd_ge80_rate"),
        "evict_dose_x": EVICT_DOSE_X,
        "note": "分层 ddr/ssd 比例仅作参考(用户前提只要求非 HBM);并发测量时不可归因单请求",
    }
    out["criteria"] = {
        "V1": {"pass": verdict_v1, "detail": v1, "canary": canary,
               "hbm_zero_rate": off_arm.get("hbm_zero_rate"),
               "off_arm": off_arm},
        "V1b": {"pass": verdict_v1b, "detail": v1b},
        "V2": {"pass": verdict_v2, "detail": v2},
        "V3": {"pass": verdict_v3, "detail": v3,
               "prefetch_wall_mean": on_arm.get("prefetch_wall_mean"),
               "prefetch_wall_max": on_arm.get("prefetch_wall_max")},
        "V4": v4,
    }
    if not verdict_v1:
        out["hint"] = ("V1(机制前提)未过:OFF 臂仍有 HBM 命中,说明压力剂量不够。"
                       "先提高 --pressure-per-rank 或 --pressure-passes 后重跑,"
                       "不要据此宣布'预取无效'。")
    elif not verdict_v2:
        out["hint"] = ("V2(收益)未过:先看 OFF 臂是否真冷(`cold_valid_rate`≈100%)与两臂 "
                       "Σ`load` 是否严格相等;再看整批墙钟/ΣTTFT 是否同向。"
                       "若整体无差,说明该配置下预取没吃到收益,而不是'统计不显著'。")
    out["verdict"] = ("PASS" if (verdict_v1 and verdict_v1b and verdict_v2 and verdict_v3)
                      else "FAIL")
    return out


def _fleet_wall(rows):
    """整批墙钟:同一被测轮里,**最早一个请求发出 → 最后一个请求答完**。

    `ts` 是 Recorder 落行时刻(= 该请求答完),故完成时刻 = ts、发出时刻 = ts − elapsed。
    """
    if not rows:
        return None
    return round(max(r["ts"] for r in rows) - min(r["ts"] - r["elapsed_s"] for r in rows), 3)


def _ratio(a, b):
    return round(a / b, 3) if (a is not None and b) else None


def _rate(vals, thr):
    vals = [v for v in vals if v is not None]
    return (sum(1 for v in vals if v >= thr) / len(vals)) if vals else None


def _hist(vals):
    vals = [v for v in vals if v is not None]
    return {"n": len(vals), "min": min(vals) if vals else None,
            "median": med(vals), "max": max(vals) if vals else None}


def _count_flags(rows):
    c: dict = {}
    for r in rows:
        for fl in r.get("flags") or []:
            c[fl] = c.get(fl, 0) + 1
    return c


# ===== 报告 =====

def write_reports(args, rec: Recorder, summary, arms_info) -> tuple:
    base = os.path.join(args.out_dir, f"agent_scenario_{args.run_id}")
    os.makedirs(args.out_dir, exist_ok=True)
    payload = {"run_id": args.run_id, "args": vars(args), "arms": arms_info,
               "summary": summary, "n_records": len(rec.records)}
    with open(base + ".json", "w", encoding="utf-8") as f:
        json.dump(payload, f, ensure_ascii=False, indent=1, default=str)

    s = summary
    ps = s["paired_stats"]
    os_ = s.get("order_stats") or {}
    canary = (s["criteria"]["V1"].get("canary") or {})
    L = []
    L.append(f"# 真实 agent 场景预取收益验证（run {args.run_id}）\n")
    L.append(f"- 结论：**{s['verdict']}**")
    L.append(f"- 流量：{args.sessions} 会话 × {args.turns} 轮"
             f"（{args.first_prompt} + (t-1)×{args.turn_growth} tok，工具间隙 {args.tool_gap}s）"
             f"，两臂 sid 相同 / nonce 不同")
    L.append(f"- 压力：{args.pressure_passes} 趟 × 池内条目，定向到各会话 rank；"
             f"需求剂量 {EVICT_DOSE_X}x 池，达成率 {[d.get('worst_margin') for a in arms_info for d in a.get('doses', [])]}")
    L.append(f"- 环境容量：DDR 总量 **{args.ddr_total_gb:g}GB** → FA(预取缓存) "
             f"**{args.fa_gib:g}GiB**(hma 对半切 × 余量 {args.ddr_margin:g})；"
             f"压力素材前 **{args.pressure_entries or '全部'}** 条(按容量自动推导)")
    L.append("")
    L.append("## 1. 用户前提（HBM 是否被挤出）\n")
    L.append(f"- OFF 臂被测样本 `hbm==0` 占比：**{_pct(s['by_arm'].get('off', {}).get('hbm_zero_rate'))}**"
             f"（{s['by_arm'].get('off', {}).get('n')} 样本）")
    L.append(f"- OFF 臂 `hbm==0 且 load>0`（冷样本成立）占比："
             f"**{_pct(s['by_arm'].get('off', {}).get('cold_valid_rate'))}**")
    L.append(f"- canary（压力前 hbm≈prompt、压力后重放）：`hbm==0` 的 rank = "
             f"{canary.get('hbm_zero_ranks')}；`hbm_ratio<=20%` 的 rank = {canary.get('hbm_low_ranks')}"
             f"；活动 rank = {canary.get('active_ranks')}")
    L.append(f"- OFF 臂 metrics `ssd_ratio`（Δwait/Δtotal）中位 "
             f"{_pct(s['by_arm'].get('off', {}).get('ssd_med'))}，"
             f"`ssd_ratio>=0.8` 占比 {_pct(s['by_arm'].get('off', {}).get('ssd_ge80_rate'))}"
             f"（**仅参考**：前提只要求取数不在 HBM，DDR/SSD 皆可）")
    L.append(f"- ON 臂 metrics `ddr_ratio` 中位 "
             f"{_pct((s['by_arm'].get('on', {}).get('ddr_hist') or {}).get('median'))}，"
             f"`ddr_ratio>=0.8` 占比 {_pct(s['by_arm'].get('on', {}).get('ddr_ge80_rate'))}")
    L.append("")
    L.append("## 2. 预取收益（预取开/关 A/B）\n")
    L.append("### 2.1 整批口径（与请求次序无关，V2 加固项）\n")
    L.append("| 轮 | 长度(tok) | n | 首字延迟中位 | 整批墙钟 | ΣTTFT（机器总请求时间） |")
    L.append("|---|---|---|---|---|---|")
    for turn, g in sorted(s.get("aggregate", {}).items()):
        L.append(
            f"| {turn} | {_fmt(g.get('prompt_tokens'))} | {g.get('n')} | "
            f"{_fmt(g.get('off_median_ttft_s'), 2)}s → {_fmt(g.get('on_median_ttft_s'), 2)}s "
            f"(**{_fmt(g.get('median_ttft_speedup'), 2)}x**，省 "
            f"{_fmt(g.get('median_ttft_saved_s'), 2)}s) | "
            f"{_fmt(g.get('off_fleet_wall_s'), 2)}s → {_fmt(g.get('on_fleet_wall_s'), 2)}s "
            f"(**{_fmt(g.get('fleet_wall_speedup'), 2)}x**) | "
            f"{_fmt(g.get('off_sum_ttft_s'), 1)}s → {_fmt(g.get('on_sum_ttft_s'), 1)}s "
            f"(**{_fmt(g.get('sum_ttft_speedup'), 2)}x**) |")
    L.append("")
    L.append("### 2.2 次序配对（「第 i 个答完的请求」，V2 主口径）\n")
    L.append(f"两臂各自按完成时刻排序后逐位配对：n={os_['n']}，"
             f"中位省 **{_fmt(os_['median_saved_s'], 2)}s**、"
             f"中位加速 **{_fmt(os_['median_speedup'], 2)}x**、"
             f"均值省 {_fmt(os_['mean_saved_s'], 2)}s，"
             f"bootstrap(1000) 95% CI "
             f"[{_fmt(os_['ci_low'], 2)}, {_fmt(os_['ci_high'], 2)}]s\n")
    L.append("| 轮 | 次序 | OFF 会话 | ON 会话 | OFF (s) | ON (s) | 省时 (s) | 加速 |")
    L.append("|---|---|---|---|---|---|---|---|")
    for p in s.get("paired_by_order") or []:
        L.append(f"| {p['turn']} | 第 {p['order']} 个 | s{p['off_k']} | s{p['on_k']} | "
                 f"{_fmt(p['off_s'], 2)} | {_fmt(p['on_s'], 2)} | "
                 f"**{_fmt(p['saved_s'], 2)}** | {_fmt(p['speedup'], 2)}x |")
    L.append("")
    _neg = next((p["saved_s"] for p in (s.get("paired") or [])
                 if p["off_s"] - p["on_s"] < 0), None)
    _why = (f"本轮同会话配对里就出现了 {_fmt(_neg, 2)}s 这样的假负样本"
            if _neg is not None else
            f"本轮同会话配对中位只有 {_fmt(ps['median_saved_s'], 2)}s，明显低于次序口径")
    L.append("> 为什么不用「同会话配对」：6 个会话**同时**发出，引擎在同一张卡上串行处理，"
             "谁先谁后由引擎调度决定、两臂之间不可控；按 sid 配对会把「排队位次」的差异"
             f"当成收益（{_why}）。次序配对与整批口径都与位次无关。\n")
    L.append("### 2.3 同会话配对（仅参考，不参与判定）\n")
    L.append(f"- n={ps['n']}，中位省 {_fmt(ps['median_saved_s'], 2)}s / "
             f"中位加速 {_fmt(ps['median_speedup'], 2)}x，"
             f"bootstrap 95% CI [{_fmt(ps['ci_low'], 2)}, {_fmt(ps['ci_high'], 2)}]s")
    L.append("")
    L.append(f"- 预取开销：mean {_fmt(s['by_arm'].get('on', {}).get('prefetch_wall_mean'), 2)}s / "
             f"max {_fmt(s['by_arm'].get('on', {}).get('prefetch_wall_max'), 2)}s"
             f"（工具间隙 {args.tool_gap}s）")
    L.append("")
    L.append("## 3. 判据\n")
    L.append("| 判据 | 结果 | 明细 |")
    L.append("|---|---|---|")
    for key in ("V1", "V1b", "V2", "V3"):
        c = s["criteria"][key]
        L.append(f"| {key} | {'PASS' if c['pass'] else 'FAIL'} | `{json.dumps(c['detail'], ensure_ascii=False)}` |")
    L.append(f"| V4 剂量-响应 | {'—' if not s['criteria']['V4'] else s['criteria']['V4'].get('pass')} | "
             f"`{json.dumps(s['criteria']['V4'] or {}, ensure_ascii=False)}` |")
    if s.get("hint"):
        L.append(f"\n> **处置建议**：{s['hint']}")
    L.append("")
    L.append("## 4. 逐臂明细\n")
    for arm in ("off", "on"):
        a = s["by_arm"].get(arm, {})
        L.append(f"### 臂 {arm}")
        L.append(f"- n={a.get('n')}  中位 TTFT {_fmt(a.get('median_ttft_s'), 2)}s"
                 f"  hbm==0 占比 {_pct(a.get('hbm_zero_rate'))}")
        L.append(f"- ddr_ratio 分布 `{json.dumps(a.get('ddr_hist'), ensure_ascii=False)}`")
        L.append(f"- flag 计数 `{json.dumps(a.get('flag_counts'), ensure_ascii=False)}`")
        L.append("")
    L.append("## 5. sid→rank 与剂量\n")
    for a in arms_info:
        L.append(f"- 臂 {a['arm']}：每 rank 会话数 `{json.dumps(a.get('rank_load'), ensure_ascii=False)}`")
        for d in a.get("doses", []):
            L.append(f"  - turn {d['turn']}：投递 {d.get('deliveries')} 条 / wall {d.get('wall_s')}s / "
                     f"最差达成率 {d.get('worst_margin')}x（需求 {EVICT_DOSE_X}x 池） / "
                     f"每 rank 剂量 `{[r.get('dose_x_pool') for r in (d.get('rows') or [])]}`x 池 / "
                     f"HBM 池峰值 `{json.dumps(d.get('pool_peak'), ensure_ascii=False)}`")
    md_path = base + ".md"
    with open(md_path, "w", encoding="utf-8") as f:
        f.write("\n".join(L) + "\n")
    return base + ".json", md_path


def _fmt(v, nd=0):
    if v is None:
        return "-"
    return f"{v:.{nd}f}"


# ===== main =====

def main() -> int:
    ap = argparse.ArgumentParser(
        description="UCM 预取收益验证（真实 agent 场景：多会话×多轮×工具间隙 + 定向压力挤 HBM）")
    ap.add_argument("--vllm-url", default=os.environ.get("UCM_ROUTER_URL",
                                                         "http://127.0.0.1:9003"),
                    help="router（推理+预取共用）。默认 9003")
    ap.add_argument("--metrics-url", default=os.environ.get(
        "UCM_METRICS_URL", "http://127.0.0.1:9202/metrics"),
        help="/metrics 地址。默认走 proxy:经源端口钉住的连接恒为活引擎的完整注册表")
    ap.add_argument("--model", default="deepseek")
    ap.add_argument("--pin-rank", type=int, default=0,
                    help="并发会话钉到的 rank(默认 0,单卡视角:压力只需覆盖 1 张卡 ⇒ 两臂 ~9 分钟)。"
                         "-1 = 不固定(会话按 consistent_hash 分散到多卡,压力覆盖全部活动 rank,"
                         "回源跨卡争抢更凶、倍数更大,但压力相位成倍变长)")
    ap.add_argument("--sessions", type=int, default=6, help="会话数（默认 6）")
    ap.add_argument("--turns", type=int, default=3, help="每会话轮数（默认 3）")
    ap.add_argument("--first-prompt", type=int, default=65536, help="首轮 token 数（默认 64k）")
    ap.add_argument("--turn-growth", type=int, default=32768, help="每轮增长 token 数（默认 ⇒ 64k/96k/128k）")
    ap.add_argument("--measure-turns", default="3", help="计时轮")
    ap.add_argument("--advance-concurrency", type=int, default=0,
                    help="非被测轮(advance)的会话并发;0=全部同时起(默认,真实车队就是同时在跑,"
                         "且聚合存活 KV 远小于 HBM 池,不会互相挤);1=串行(全集群版旧行为)")
    ap.add_argument("--measure-concurrency", type=int, default=0,
                    help="被测轮的会话并发;0=全会话同时起(真实 agent 多会话在跑,"
                         "聚合存活 KV 才能超过 HBM 池);1=串行(仅用于干净的 /metrics 归因)")
    ap.add_argument("--pressure-turns", default="3", help="施加压力的轮（须 ⊆ measure-turns）")
    ap.add_argument("--pressure-passes", type=int, default=3, help="压力趟数")
    ap.add_argument("--pressure-per-rank", type=int, default=48,
                    help="每 rank 总投递条数(默认 48 x 262144 tok = 6x HBM 池;"
                         "1.5x 挤不动 LRU-by-touch 的会话前缀,4~6x 才使 canary hbm==0)")
    ap.add_argument("--pressure-entries", type=int, default=-1,
                    help="压力只用池的前 N 条(-1=按 DDR 容量自动推导,推荐;0=全部;>0=显式)。"
                         "**不是随便取的**:压力步要能把目标前缀挤出 DDR 的 FA 缓存,素材体量须"
                         " >= FA 容量 × --ddr-margin。自动值 = ceil(FA容量×余量 / 单条体量);"
                         "示例 DDR=64GB ⇒ FA 32GiB ⇒ 需 ~38.4GiB ⇒ 单条 1.49GiB 时 26 条。"
                         "注意:取全部 34 条不会更有效,只会把每趟 SSD 读量放大 ~1.3x(空耗时长)")
    ap.add_argument("--pressure-concurrency", type=int, default=0,
                    help="每趟并发上限;0=不限(整轮同时起,使每 rank 在飞=该 rank 的 sid 数)")
    ap.add_argument("--hbm-canary", type=int, default=1, help="1=用专用 canary 前缀验证 HBM 已被压力挤出")
    ap.add_argument("--canary-tokens", type=int, default=16384,
                    help="专用 canary 前缀长度。**必须与被测前缀不同内容**:canary 跑在测量之前,"
                         "若重放被测前缀会把命中块灌回 DDR、抹平 OFF 臂冷启动(会吃掉约 67%% 收益)")
    ap.add_argument("--probe-per-rank", type=int, default=1,
                    help="canary / 落盘探针每 rank 抽查几个会话(0=全部)。二者证明的是 rank 级"
                         "属性(同 rank 会话共用一个 KV pool),逐会话重放只是重复同一结论并空等")
    ap.add_argument("--dump-settle", type=float, default=10.0,
                    help="turn1 之后等 dump 落盘的时长(s)。后面还有 helper 探测 + turn2 + 落盘探针 + "
                         "压力共 ~2.5 分钟也是落盘窗口,故这里不用等太久")
    ap.add_argument("--tool-gap", type=float, default=5.0, help="工具间隙(s)，也是 V3 上限")
    ap.add_argument("--settle-after-pressure", type=float, default=3.0)
    ap.add_argument("--settle-between-arms", type=float, default=15.0)
    ap.add_argument("--arm-order", default="off,on")
    ap.add_argument("--pool-file", default=POOL_G1_DEFAULT)
    # DDR 容量:随部署不同,与容量挂钩的参数(压力素材条数)都按它推导
    ap.add_argument("--cache-buffer-capacity-gb", type=float, default=0.0,
                    help="UCM 的 cache_buffer_capacity_gb(**节点级** DDR 缓存总量,GiB)。"
                         "0=读环境变量 UCM_CACHE_BUFFER_CAPACITY_GB,再不行用 64")
    ap.add_argument("--fa-buffer-gib", type=float, default=0.0,
                    help="FA(预取缓存)侧容量 GiB;0=总量对半切(hma_connector 默认行为)")
    ap.add_argument("--ddr-margin", type=float, default=1.20,
                    help="挤 DDR 的余量系数:要求 压力素材体量 >= FA 容量 × 该值(默认 1.20)")
    ap.add_argument("--flush-tokens", type=int, default=262144)
    ap.add_argument("--block-bytes", type=int, default=BLOCK_BYTES_DEFAULT)
    ap.add_argument("--warm-chunk", type=int, default=128)
    ap.add_argument("--max-tokens", type=int, default=1)
    ap.add_argument("--timeout", type=float, default=900.0)
    ap.add_argument("--log-dir", default=LOG_DIR_DEFAULT)
    ap.add_argument("--log-wait-tries", type=int, default=8)
    ap.add_argument("--log-wait-s", type=float, default=0.5)
    ap.add_argument("--no-drop-caches", action="store_true")
    ap.add_argument("--out-dir", default=REPORTS_DIR_DEFAULT)
    ap.add_argument("--preflight-only", action="store_true")
    ap.add_argument("--pressure-scan", default="", help="如 1,2,3：只跑 OFF 臂做剂量-响应")
    ap.add_argument("--dp-size", type=int,
                    default=int(os.environ.get("UCM_DP_SIZE", "4")),
                    help="引擎 DP 路数;preflight 要求 /metrics 覆盖 rank 0..dp_size-1(默认 4)")
    ap.add_argument("--hbm-pool-tokens", type=int,
                    default=int(os.environ.get("UCM_HBM_POOL_TOKENS", "0")),
                    help="每 DP rank 的 vLLM KV 池 token 数;0=用内置默认 2083204(TP4/DP4 默认口径)。"
                         "TP8/DP1 为 558665(显存利用率 0.9、512k 上下文)")
    args = ap.parse_args()
    if args.hbm_pool_tokens:
        globals()["HBM_TOKENS_PER_RANK"] = args.hbm_pool_tokens
        print(f"[cfg] HBM_TOKENS_PER_RANK 覆盖为 {args.hbm_pool_tokens}"
              f"(--hbm-pool-tokens / UCM_HBM_POOL_TOKENS)")


    args.run_id = f"{int(time.time())}-{random.randrange(1 << 24):06x}"
    args.arm = "off"
    args.scan_max = 0
    args.pinned_sids = []          # --pin-rank >=0 时在下方探测填充(两臂共用同一批)
    args.log_files = log_files_for(args.log_dir)
    args.turn_lens = {t: args.first_prompt + (t - 1) * args.turn_growth
                      for t in range(1, args.turns + 1)}
    args.measure_turns = {int(x) for x in args.measure_turns.split(",") if x.strip()}
    args.pressure_turns = {int(x) for x in args.pressure_turns.split(",") if x.strip()}
    if not args.pressure_turns <= args.measure_turns:
        print(f"[FAIL] --pressure-turns {sorted(args.pressure_turns)} 必须 ⊆ "
              f"--measure-turns {sorted(args.measure_turns)}")
        return 1

    if args.preflight_only:
        print(f"== preflight  run={args.run_id}  worker 日志: {args.log_files}")
        return preflight(args)

    if not os.path.exists(args.pool_file):
        print(f"[FAIL] 池文件不存在: {args.pool_file}（压力步依赖它）")
        return 1
    pool = pool_load(args.pool_file)
    if pool.get("flush_tokens") != args.flush_tokens:
        print(f"[FAIL] 池 flush_tokens={pool.get('flush_tokens')} != --flush-tokens={args.flush_tokens}")
        return 1
    resolve_ddr(args)   # DDR 容量 -> FA 侧容量(压力素材条数按它推导)
    if resolve_pressure_entries(args, pool):
        return 1

    scan = [int(x) for x in args.pressure_scan.split(",") if x.strip()]
    args.scan_max = max(scan) if scan else 0
    os.makedirs(args.out_dir, exist_ok=True)
    rec = Recorder(os.path.join(args.out_dir, f"agent_scenario_{args.run_id}.jsonl"))
    print(f"== 真实 agent 场景预取收益验证  run={args.run_id}")
    print(f"   router={args.vllm_url}  metrics={args.metrics_url}")
    print(f"   {args.sessions} 会话 × {args.turns} 轮（L={list(args.turn_lens.values())}）"
          f"  被测轮={sorted(args.measure_turns)} 压力轮={sorted(args.pressure_turns)}")
    print(f"   压力={args.pressure_passes} 趟 × 池 {len(pool['entries'])} 条"
          f"  工具间隙={args.tool_gap}s  dump-settle={args.dump_settle}s")
    print(f"   HBM/rank={HBM_TOKENS_PER_RANK} tok  block={args.block_bytes}B"
          f"  worker 日志: {args.log_files}")
    print(f"   产物: {args.out_dir}/agent_scenario_{args.run_id}.{{log,jsonl,json,md}}", flush=True)

    # 单卡视角:先探出 N 个落点相同的 sid,两臂共用(rank 相同 -> 配对消差)。
    if args.pin_rank >= 0:
        print(f"\n== 固定落点:探测 {args.sessions} 个落在 rank{args.pin_rank} 的 sid ...", flush=True)
        args.pinned_sids = find_pinned_sids(args, args.pin_rank, args.sessions)
        if len(args.pinned_sids) < args.sessions:
            print(f"[FAIL] 只探到 {len(args.pinned_sids)}/{args.sessions} 个 sid(consistent_hash"
                  f" 分布过于偏斜);可换 --pin-rank 或减少 --sessions")
            return 1
        print(f"   pinned_sids={args.pinned_sids}", flush=True)

    arms_info = []
    v4 = None
    try:
        if scan:
            print(f"\n== 剂量-扫描模式（只跑 OFF 臂）passes={scan}")
            ratios = []
            for p in scan:
                args.pressure_passes = p
                start = len(rec.records)
                info = arm_run(args, "off", pool, rec)
                arms_info.append(info)
                rows = [r for r in rec.records[start:]
                        if r.get("phase") == "measure" and r["arm"] == "off"]
                zero = (sum(1 for r in rows if (r.get("hbm") or 0) == 0) / len(rows)) if rows else 0
                ratios.append({"passes": p, "n": len(rows), "hbm_zero_rate": round(zero, 4),
                               "worst_margin": [d.get("worst_margin") for d in info.get("doses", [])]})
                print(f"  [scan] passes={p}: hbm==0 占比 {zero*100:.0f}% (n={len(rows)})", flush=True)
                time.sleep(args.settle_between_arms)
            mono = all(b["hbm_zero_rate"] >= a["hbm_zero_rate"] - 1e-9
                       for a, b in zip(ratios, ratios[1:]))
            v4 = {"scan": ratios, "monotonic": mono, "pass": mono}
        else:
            for arm in [a.strip() for a in args.arm_order.split(",") if a.strip()]:
                if arm not in ("off", "on"):
                    continue
                arms_info.append(arm_run(args, arm, pool, rec))
                time.sleep(args.settle_between_arms)
    except KeyboardInterrupt:
        print("\n[warn] 被中断，输出已有记录的部分结果")
    finally:
        summary = summarize(args, arms_info, rec.records, v4)
        json_path, md_path = write_reports(args, rec, summary, arms_info)
        rec.close()
        print(f"\n== 结果: {summary['verdict']}  ->  {md_path}")
        print(f"   json={json_path}  jsonl={rec.path}")
        for key in ("V1", "V1b", "V2", "V3"):
            c = summary["criteria"][key]
            print(f"   {key}: {'PASS' if c['pass'] else 'FAIL'} {json.dumps(c['detail'], ensure_ascii=False)}")
        if summary.get("hint"):
            print(f"   [hint] {summary['hint']}")
        os_ = summary["order_stats"]
        print(f"   次序配对 n={os_['n']} 中位省时={_fmt(os_['median_saved_s'], 3)}s"
              f" 加速={_fmt(os_['median_speedup'], 2)}x CI=[{_fmt(os_['ci_low'], 3)},"
              f"{_fmt(os_['ci_high'], 3)}]")
        for turn, g in sorted(summary.get("aggregate", {}).items()):
            print(f"   整批口径 turn{turn}: 首字中位 {_fmt(g['off_median_ttft_s'], 2)}→"
                  f"{_fmt(g['on_median_ttft_s'], 2)}s({_fmt(g['median_ttft_speedup'], 2)}x)"
                  f"  墙钟 {_fmt(g['off_fleet_wall_s'], 2)}→{_fmt(g['on_fleet_wall_s'], 2)}s"
                  f"({_fmt(g['fleet_wall_speedup'], 2)}x)"
                  f"  ΣTTFT {_fmt(g['off_sum_ttft_s'], 1)}→{_fmt(g['on_sum_ttft_s'], 1)}s"
                  f"({_fmt(g['sum_ttft_speedup'], 2)}x)")
        ps = summary["paired_stats"]
        print(f"   [参考]同会话配对 n={ps['n']} 中位省时={_fmt(ps['median_saved_s'], 3)}s"
              f" 加速={_fmt(ps['median_speedup'], 2)}x CI=[{_fmt(ps['ci_low'], 3)},"
              f"{_fmt(ps['ci_high'], 3)}]（位次不可控,不参与判定）")
    return 0 if summary["verdict"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())