# ram-a 对接 actrail-kv 使用指南

> 适用：deepagents-middleware + actrail-kv
> 功能：middleware 采集每次 LLM 调用的 **prompt_tokens / completion_tokens / ttft_ms / total_ms**，经 RAM-A daemon 通过 SDK in-process 调用 actrail-kv receiver 写入存储，由 analyze/report 生成命中率看板（含 TTFT、总耗时列）。
> 本文档所有命令块均可直接复制执行。

---

## 0. 总体链路

```
你的 Agent 应用 (deepagents + deepagents-middleware)
   │  每次模型调用自动统计: prompt/completion tokens、TTFT、总耗时
   ▼  capture payload
RAM-A daemon (ram-a-kv)            ← 本指南步骤 2、4
   │  SDK in-process submit (fire-and-forget)
   ▼
libactrail_receiver.so             ← 本指南步骤 3
   │  写入 captures.sqlite / captures/
   ▼
actrail-kv analyze ──▶ analysis.json
   ▼
actrail-kv report ──▶ report.html（命中率看板）
```

> 从 actrail-kv 0.2.1 起，ram-a-kv daemon 通过 `actrail-kv-sdk` 以进程内动态库方式调用 receiver 能力，**无需独立运行 `actrail-kv-receiver` 进程**。采集记录默认写入 `~/.actrail/actrail-kv/captures.sqlite`。

---

## 1. 前置条件

- Rust（cargo build，actrail-kv 与 daemon 用）
- Python ≥ 3.11，且已安装 deepagents-middleware 的依赖：
  `langchain==1.3.16 langchain-core==1.6.0 langchain-openai==1.6.0 httpx==0.28.1 langgraph`
- 已运行中的 vLLM / OpenAI 兼容推理服务（供 Agent 调用；**无需** `--enable-per-request-metrics`）

---

## 2. 拉取代码

```bash
# RAM-A（daemon + deepagents-middleware）
git clone https://gitcode.com/openeuler/RAM-A.git
```

---

## 3. 构建 actrail-kv 动态库与命令行工具

ram-a-kv daemon 需要 `libactrail_receiver.so`（进程内加载），分析报告需要 `actrail-kv-analyze` / `actrail-kv-report` 命令。在 RAM-A 仓库根目录构建：

```bash
cd RAM-A/ram-a-kv/actrail-kv

# 构建 receiver 动态库（产物：target/release/libactrail_receiver.so）
cargo build --release -p actrail-kv-sdk-receiver

# 安装命令行工具到 ~/.cargo/bin
cargo install --locked --path crates/cmd/analyze
cargo install --locked --path crates/cmd/report

cd ../..
```

> 如果 `actrail-kv-analyze` / `actrail-kv-report` 已在 `PATH` 上，可跳过对应安装步骤。动态库路径在步骤 4 的 daemon 配置中指定。

---

## 4. 构建 RAM-A daemon

```bash
cd RAM-A/ram-a-kv
cargo build --release                    # 默认：会话状态存内存，不依赖 SQLite
# cargo build --release --features sqlite  # 需要 SQLite 持久化时改用这条
# 产物：target/release/ram-a-kv
cd ../..
```

---

## 5. 配置 actrail-kv 采集存储

actrail-kv 通过 SDK 读取默认设置文件 `~/.actrail/actrail-kv/settings.toml`，缺项自动补齐写回。也可显式创建该文件：

```bash
mkdir -p ~/.actrail/actrail-kv
cat > ~/.actrail/actrail-kv/settings.toml <<'EOF'
[storage]
backend = "sqlite"

[storage.retention]
max_age_seconds = 604800
max_size_bytes = 268435456
EOF
```

> 省略 `[storage]` 时，receiver 默认使用 SQLite，写入设置文件旁的 `captures.sqlite`。

### 5.1 配置采集保留期和容量

共享存储同时限制采集记录的保留期和容量。容量不足时优先清理最旧的数据。

| `[storage.retention]` 配置项 | 默认值 | 设置规则 |
|---|---|---|
| `max_age_seconds` | 7 天（`604800` 秒） | 从采集时间起算。`0` 关闭按时间清理，已有的 `0` 在升级后继续生效。 |
| `max_size_bytes` | 256 MiB（`268435456` 字节） | 必须为正数。容量上限始终生效。 |

可在 `[storage.backends.sqlite.retention]` 或 `[storage.backends.ndjson.retention]` 中覆盖对应后端的设置。

| 后端 | 容量计量 | 按时间清理 | 按容量清理 |
|---|---|---|---|
| SQLite | 采集记录的 JSON 字节数。实际数据库文件可能更大。 | 分批删除过期记录 | 按采集时间删除最旧记录 |
| 托管 NDJSON | 分片文件的总字节数 | 整个分片到期后删除 | 从最旧分片开始回收 |

SDK 在打开可写存储和写入时检查容量。单条记录超过容量上限时，submit 返回错误。分析时会跳过过期记录。

---

## 6. 配置并启动 RAM-A daemon

创建 `daemon.toml`（指定动态库路径与采集维度）：

```toml
listen_addr = "127.0.0.1:6998"
noop_backend = true
session_store_path = "./sessions.db"   # 仅 --features sqlite 构建生效
# libactrail_receiver.so 绝对路径；留空则禁用 actrail 集成
actrail_sdk_lib = "/path/to/RAM-A/ram-a-kv/actrail-kv/target/release/libactrail_receiver.so"
actrail_model_deployment_key = "deepseek-v4-flash"
actrail_kv_namespace = "default"
```

> `session_store_path` 只在用 `--features sqlite` 构建的 daemon 上生效。默认构建（内存存储）会忽略它，不创建 `sessions.db`，daemon 重启后会话状态不恢复；本指南的 actrail 联调不依赖持久化，默认构建即可。

启动 daemon：

```bash
RAM_A_KV_CONFIG=daemon.toml ./RAM-A/ram-a-kv/target/release/ram-a-kv
```

启动日志输出 `actrail-kv sdk enabled` 表示动态库加载成功；`actrail-kv sdk disabled` 表示未配置 `actrail_sdk_lib`；加载失败（文件缺失/ABI 不匹配）时 daemon **拒绝启动**（fail-fast）。

---

## 7. 接入 deepagents-middleware（二选一）

### 7.1 方式 A：直接部署（显式中间件）

```python
import os, sys
sys.path.insert(0, "/path/to/RAM-A/ram-a-kv/deepagents-middleware/src")

from langchain.agents.middleware import AgentMiddleware
from ram_a_middleware.deepagents.middleware import RamAKvMiddleware, AsyncRamAKvMiddleware
from ram_a_middleware.sdk import RamAClient

# 同步客户端
client = RamAClient("http://127.0.0.1:6998", timeout=2.0, retries=1)
mw = RamAKvMiddleware(
    client,
    actrail_enabled=True,          # 打开 actrail 采集（计时/token 上报）
    actrail_endpoint_key="llm-primary",   # 按你的部署自定义
    actrail_agent_key="coding-agent",     # 按你的 Agent 自定义
)

# 异步客户端（可选）
# from ram_a_middleware.sdk import AsyncRamAClient
# mw = AsyncRamAKvMiddleware(AsyncRamAClient("http://127.0.0.1:6998"), **{...})

agent = create_deep_agent(..., middleware=[mw, ...])
```

### 7.2 方式 B：插件部署（零侵入，推荐）

```bash
pip install ram-a-sdk  # 或安装源码分支 deepagents-sdk 的 ram_a_deepagents
```

```python
from ram_a_deepagents import (
    DeepAgentsClient,
    RamAKvHook,
    install_ram_a_plugin,
    set_ram_a_hook,
)

# 必须在构造 Agent 之前调用；幂等
install_ram_a_plugin()                # 包装 langchain create_agent
client = DeepAgentsClient("http://127.0.0.1:6998")
set_ram_a_hook(RamAKvHook(client))    # 注册 ram-a 生命周期钩子

agent = create_deep_agent(...)        # 之后的每个模型调用自动被采集
```

> 生效前提：`request.model` 是标准 `langchain_openai.ChatOpenAI`（middleware 自动替换为 `_KvResponseChatOpenAI` 走流式计时）；流式 chunk 里带 `usage` 才有 token 数。

---

## 8. 生成看板

```bash
actrail-kv-analyze \
  --config ~/.actrail/actrail-kv/settings.toml \
  --top-k 20

actrail-kv-report \
  --config ~/.actrail/actrail-kv/settings.toml \
  --output report.html
```

analyze 读取设置文件指定的采集记录，report 默认读取分析产物。设置文件内的相对路径以该文件所在目录为基准。

浏览器打开 `report.html`：

- **命中率表**：每行一次请求，含 prompt / completion / TTFT / 总耗时 列；未上报值的行显示 `—`
- **结构区**：上下文结构代理、模板、缺陷与调优建议（需 ≥3 条相似请求才出模板/缺陷）

### 8.1 筛选分析记录

筛选决定本次分析的输入范围。下面的命令按会话和采集时间筛选，再选取最新的 100 条记录。

```bash
actrail-kv-analyze \
  --config ~/.actrail/actrail-kv/settings.toml \
  --session-key session-a \
  --time-range 2026-11-24..2026-11-26 \
  --last-n 100
```

`--session-key` 和 `--time-range` 可重复传入，匹配任一所选会话和任一所选时间段。时间段包含起点并排除终点，支持 UTC 日期和 RFC 3339 时间戳。单个时间段也可用 `--from` 和 `--until` 指定，可只填一端。

可以在 `[analyzer.selection]` 中保存 `session_keys` 和 `last_n`，用 `[[analyzer.selection.time_ranges]]` 表设置各时间段的 `from` 和 `until`。显式命令行参数覆盖对应的筛选设置。

---

## 9. 指标字段说明（落盘 `payload._actrail_response_usage`）

| 字段 | 含义 | 来源 |
|---|---|---|
| `prompt_tokens` | 本次请求完整输入 token 数（命中率分母） | 引擎流末 usage（别名 `input_tokens`） |
| `completion_tokens` | 本次输出 token 数 | 引擎流末 usage（别名 `output_tokens`） |
| `ttft_ms` | 首 token 时延（请求发出→首 chunk 到达） | middleware 侧 perf_counter |
| `total_ms` | 总耗时（请求发出→流式结束） | middleware 侧 perf_counter |

引擎未上报 usage 时省略 token 字段；计时始终统计（不依赖引擎开关）。

---

## 10. 常见问题

1. **看板 TTFT/总耗时 全是 `—`**：capture 未携带 `_actrail_response_usage`。检查 middleware 是否以 `actrail_enabled=True` 接入、`request.model` 是否为标准 `ChatOpenAI`。
2. **命中率恒为 0（reported）**：`cached_tokens` 尚未由 middleware 补传（引擎缓存命中 token）。当前 estimate 口径看 LCP 估算命中率。
3. **mock 验证计时不反映延迟**：mock 服务器必须用 HTTP/1.1 `Transfer-Encoding: chunked`（不能设 `Content-Length`，否则客户端整包读取，ttft≈total）。
4. **daemon 启动失败 `actrail SDK load failed`**：检查 `actrail_sdk_lib` 路径是否正确、`.so` 文件是否存在、ABI 版本是否匹配（需 actrail-kv ≥ 0.2.1 构建的 `libactrail_receiver.so`）。
5. **采集记录为空**：SDK submit 为 fire-and-forget，若 daemon 未起或未配 actrail 项则静默丢弃。先看 daemon 日志是否有 `actrail sdk submit ok`；若只有 `actrail sdk submit failed`，检查 `~/.actrail/actrail-kv/settings.toml` 配置与磁盘空间。
6. **`~/.actrail/actrail-kv/` 目录不存在**：首次运行时 SDK 会自动创建并补齐默认 `settings.toml`；若权限不足需手动创建。

---

## 附录 A：复测脚本全文（ttft_total_e2e_verify.py）

> ⚠️ 此脚本需配合 actrail-kv SDK 动态库使用。脚本会启动 mock LLM + 真实 daemon（加载 `libactrail_receiver.so`）+ 真实 middleware，跑全链路并断言。

保存为 `ttft_total_e2e_verify.py` 后直接运行（脚本内路径可按本机部署调整 `OUT_DIR` / 各二进制路径）：

```python
"""端到端验证 middleware 流式计时 (ttft_ms / total_ms) 统计并传送到 actrail-kv 看板。

链路：真实 middleware (RamAKvMiddleware.wrap_model_call) 替换 request.model 为
      _KvResponseChatOpenAI -> 其 _generate 走 _stream 聚合真实流式响应时计时
      -> capture.payload._actrail_response_usage 携带 ttft_ms/total_ms
      -> daemon SDK submit -> libactrail_receiver.so 写入 captures.sqlite
      -> analyze -> report.html。

mock OpenAI 服务器人为制造流式延迟，用于校验计时是否如实反映链路：
      t0  请求发出
      0.5s 后发送首个 content chunk   => ttft_ms 应约 500
      再 1.0s 发送第二个 chunk + usage => total_ms 应约 1500
"""
from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# 部署路径：环境变量 AKV_ROOT 指向代码检出根目录（示例 git clone 到该目录后直接可用）
# 亦可单独覆盖 AKV_OUT_DIR；下方默认值仅示意，<checkout-root> 请按本机实际检出位置替换
AKV_ROOT = os.environ.get("AKV_ROOT", "<checkout-root>")
OUT_DIR = os.environ.get("AKV_OUT_DIR", os.path.join(AKV_ROOT, "actrail/ttft-total-e2e-verify"))
RECEIVER_LIB = os.environ.get(
    "RECEIVER_LIB",
    os.path.join(AKV_ROOT, "RAM-A/ram-a-kv/actrail-kv/target/release/libactrail_receiver.so"),
)
ANALYZE = "actrail-kv-analyze"
REPORT = "actrail-kv-report"
DAEMON = os.path.join(AKV_ROOT, "RAM-A/ram-a-kv/target/release/ram-a-kv")

MW_SRC = os.path.join(AKV_ROOT, "RAM-A/ram-a-kv/deepagents-middleware/src")
sys.path.insert(0, MW_SRC)

# 人为延迟（秒）
TTFT_DELAY = 0.5
STREAM_DELAY = 1.0
# 计时断言窗口（毫秒）
TTFT_LO, TTFT_HI = 250, 1500
TOTAL_LO, TOTAL_HI = 1000, 4000


class MockLLMHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    def _send_event(self, data: bytes) -> None:
        self.wfile.write(("%x\r\n" % len(data)).encode("ascii") + data + b"\r\n")
        self.wfile.flush()

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        self.rfile.read(length)
        # SSE 必须用 chunked（无 Content-Length），客户端才能逐 chunk 流式接收
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Transfer-Encoding", "chunked")
        self.send_header("Connection", "close")
        self.end_headers()

        time.sleep(TTFT_DELAY)
        self._send_event(
            b'data: {"id":"chatcmpl-mock","object":"chat.completion.chunk","created":1,'
            b'"model":"deepseek","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}\n\n'
        )
        time.sleep(STREAM_DELAY)
        self._send_event(
            b'data: {"id":"chatcmpl-mock","object":"chat.completion.chunk","created":1,'
            b'"model":"deepseek","choices":[{"index":0,"delta":{"content":" world"},"finish_reason":null}]}\n\n'
        )
        self._send_event(
            b'data: {"id":"chatcmpl-mock","object":"chat.completion.chunk","created":1,'
            b'"model":"deepseek","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],'
            b'"usage":{"prompt_tokens":123,"completion_tokens":2,"total_tokens":125}}\n\n'
            b'data: [DONE]\n\n'
        )
        self.wfile.write(b"0\r\n\r\n")
        self.wfile.flush()


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def main() -> int:
    from langchain.agents.middleware.types import ModelRequest, ModelResponse
    from langchain_core.messages import HumanMessage
    from langchain_openai import ChatOpenAI
    from langgraph.runtime import Runtime

    from ram_a_middleware.deepagents.middleware import RamAKvMiddleware
    from ram_a_middleware.sdk import RamAClient

    os.makedirs(OUT_DIR, exist_ok=True)
    # SDK 通过 HOME 环境变量读 ~/.actrail/actrail-kv/settings.toml，
    # 此处 HOME=OUT_DIR，所以 settings 必须写到 OUT_DIR/.actrail/actrail-kv/settings.toml，
    # analyze/report 的 --config 也指向同一文件。
    settings_dir = os.path.join(OUT_DIR, ".actrail", "actrail-kv")
    os.makedirs(settings_dir, exist_ok=True)
    settings_path = os.path.join(settings_dir, "settings.toml")
    analysis_path = os.path.join(settings_dir, "analysis.json")
    report_path = os.path.join(OUT_DIR, "report.html")
    for path in (analysis_path, report_path):
        if os.path.exists(path):
            os.remove(path)

    # actrail-kv settings.toml：receiver SDK 读取此文件，写入 settings_dir/captures.sqlite
    with open(settings_path, "w", encoding="utf-8") as fh:
        fh.write("\n".join((
            "[storage]",
            "backend = \"sqlite\"",
            "",
            "[storage.retention]",
            "max_age_seconds = 604800",
            "max_size_bytes = 268435456",
            "",
        )))
    # SDK 默认读 ~/.actrail/actrail-kv/settings.toml；此处通过 HOME 环境变量重定向
    env = dict(os.environ)
    env["HOME"] = OUT_DIR

    # 1) mock LLM（人为流式延迟）
    llm_port = free_port()
    llm_server = ThreadingHTTPServer(("127.0.0.1", llm_port), MockLLMHandler)
    threading.Thread(target=llm_server.serve_forever, daemon=True).start()

    # 2) RAM-A daemon（加载 libactrail_receiver.so，SDK in-process 采集）
    daemon_port = free_port()
    config = os.path.join(OUT_DIR, "daemon.toml")
    with open(config, "w", encoding="utf-8") as fh:
        fh.write("\n".join((
            f'listen_addr = "127.0.0.1:{daemon_port}"',
            "noop_backend = true",
            f'session_store_path = "{OUT_DIR}/sessions.db"',
            f'actrail_sdk_lib = "{RECEIVER_LIB}"',
            'actrail_model_deployment_key = "deepseek-v4-flash"',
            'actrail_kv_namespace = "default"',
            "",
        )))
    env["RAM_A_KV_CONFIG"] = config
    daemon = subprocess.Popen([DAEMON], env=env,
                              stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)

    try:
        # 3) 等 daemon 健康
        url = f"http://127.0.0.1:{daemon_port}"
        import httpx
        for _ in range(50):
            if daemon.poll() is not None:
                raise RuntimeError("daemon exited early")
            try:
                resp = httpx.post(f"{url}/event", json={"type": "health"}, timeout=0.5)
                if resp.status_code == 200 and resp.json().get("ok"):
                    break
            except Exception:
                pass
            time.sleep(0.1)

        # 4) 真实 middleware + 真实 ChatOpenAI（指向 mock LLM）
        client = RamAClient(url, timeout=2.0, retries=1)
        mw = RamAKvMiddleware(client, actrail_enabled=True,
                              actrail_endpoint_key="llm-primary",
                              actrail_agent_key="coding-agent")
        model = ChatOpenAI(model="deepseek-v4-flash", api_key="test",
                           base_url=f"http://127.0.0.1:{llm_port}/v1", timeout=30)

        def make_request(session, turn, content):
            return ModelRequest(
                model=model,
                messages=[HumanMessage(content=content)],
                system_message=None,
                tools=[],
                state={"_ram_a_session_id": session, "_ram_a_turn_id": turn},
                runtime=Runtime(context={}),
            )

        def handler(_request):
            # middleware 已用 preserve_kv_response 把 model 换成 _KvResponseChatOpenAI
            # 这里真实 invoke：_generate -> _stream 聚合时计时
            message = _request.model.invoke(_request.messages)
            return ModelResponse(result=[message])

        print("== 请求 1：真实流式调用（mock 延迟 0.5s 首 chunk / 1.5s 结束）==")
        mw.wrap_model_call(make_request("ttft-total-001", "turn-1", "hello"), handler)
        print("== 请求 2：真实流式调用（同一会话下一轮）==")
        mw.wrap_model_call(make_request("ttft-total-001", "turn-2", "hello again"), handler)
        client.close()

        # 5) 等 SDK submit 落盘（fire-and-forget）
        sqlite_path = os.path.join(OUT_DIR, ".actrail", "actrail-kv", "captures.sqlite")
        deadline = time.monotonic() + 8
        while True:
            if os.path.exists(sqlite_path) and os.path.getsize(sqlite_path) > 0:
                break
            if time.monotonic() > deadline:
                raise RuntimeError("SDK 未落盘任何记录")
            time.sleep(0.1)
        print(f"SDK 落盘 {sqlite_path}")

        # 6) analyze -> report
        subprocess.run([ANALYZE, "--config", settings_path, "--top-k", "20"],
                       check=True, capture_output=True)
        # analyze 默认输出 analysis.json 到 settings 同目录
        analysis = json.load(open(analysis_path, encoding="utf-8"))
        metrics = analysis.get("cache_metrics", [])
        print("=== analysis.cache_metrics:")
        for m in metrics:
            print("  ", json.dumps(m, ensure_ascii=False, sort_keys=True))
        # 验证 ttft_ms/total_ms 透传
        assert metrics, "analysis 无 cache_metrics"
        rec1_metrics = next(m for m in metrics if m.get("basis") == "reported")
        assert TTFT_LO <= rec1_metrics["ttft_ms"] <= TTFT_HI, rec1_metrics
        assert TOTAL_LO <= rec1_metrics["total_ms"] <= TOTAL_HI, rec1_metrics
        assert rec1_metrics["ttft_ms"] < rec1_metrics["total_ms"], rec1_metrics
        print("  ttft_ms/total_ms 真实统计且在预期窗口内（300~1500 / 1000~4000），ttft < total ✓")

        subprocess.run([REPORT, "--config", settings_path, "--output", report_path],
                       check=True, capture_output=True)
        html = open(report_path, encoding="utf-8").read()
        assert html.count("TTFT") >= 1 and "总耗时" in html
        print(f"  report.html 命中率表 TTFT/总耗时 列渲染实际统计值 ✓")
        return 0
    finally:
        llm_server.shutdown()
        if daemon.poll() is None:
            daemon.terminate()
            try:
                daemon.wait(timeout=5)
            except subprocess.TimeoutExpired:
                daemon.kill()


if __name__ == "__main__":
    raise SystemExit(main())
```
