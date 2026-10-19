# ram-a 对接 actrail-kv 使用指南

> 适用：deepagents-middleware + actrail-kv（分支 `feat/kv-cache-metrics`）；版本以仓库最新为准
> 功能：middleware 采集每次 LLM 调用的 **prompt_tokens / completion_tokens / ttft_ms / total_ms**，经 RAM-A daemon relay 转发到 actrail-kv，落盘后由 analyze/report 生成命中率看板（含 TTFT、总耗时列）。
> 本文档所有命令块均可直接复制执行。

---

## 0. 总体链路

```
你的 Agent 应用 (deepagents + deepagents-middleware)
   │  每次模型调用自动统计: prompt/completion tokens、TTFT、总耗时
   ▼  capture payload
RAM-A daemon relay (ram-a-kv)   ← 本指南步骤 2
   │  POST /event (fire-and-forget)
   ▼
actrail-kv receiver            ← 本指南步骤 3
   │  追加写入 requests.ndjson
   ▼
actrail-kv analyze ──▶ analysis.json
   ▼
actrail-kv report ──▶ report.html（命中率看板）
```

---

## 1. 前置条件

- Rust（cargo build，actrail-kv 与 daemon 用）
- Python ≥ 3.11，且已安装 deepagents-middleware 的依赖：
  `langchain==1.3.16 langchain-core==1.6.0 langchain-openai==1.6.0 httpx==0.28.1 langgraph`
- 已运行中的 vLLM / OpenAI 兼容推理服务（供 Agent 调用；**无需** `--enable-per-request-metrics`）

---

## 2. 拉取代码

```bash
# actrail-kv（分析 + 看板）
git clone https://gitcode.com/yaozhenhe/actrail-kv.git
cd actrail-kv
git checkout feat/kv-cache-metrics
cd ..

# RAM-A（daemon relay + deepagents-middleware）
git clone https://gitcode.com/openeuler/RAM-A.git
```

---

## 3. 构建 actrail-kv（receiver / analyze / report）

```bash
cd actrail-kv
cargo build --release
# 产物：
#   target/release/actrail-kv-receiver
#   target/release/actrail-kv-analyze
#   target/release/actrail-kv-report
cd ..
```

---

## 4. 构建并配置 RAM-A daemon relay

```bash
cd RAM-A/ram-a-kv
cargo build --release
# 产物：target/release/ram-a-kv
cd ../..
```

创建 `daemon.toml`（改监听端口与转发目标）：

```toml
listen_addr = "127.0.0.1:6998"
noop_backend = true
session_store_path = "./sessions.db"
actrail_receiver_url = "http://127.0.0.1:8087"
actrail_model_deployment_key = "deepseek-v4-flash"
actrail_kv_namespace = "default"
```

启动 daemon：

```bash
RAM_A_KV_CONFIG=daemon.toml ./RAM-A/ram-a-kv/target/release/ram-a-kv
```

---

## 5. 启动 actrail-kv receiver

```bash
./actrail-kv/target/release/actrail-kv-receiver \
  --listen 127.0.0.1:8087 \
  --output requests.ndjson
```

> 端口须与 `daemon.toml` 的 `actrail_receiver_url` 一致（此处 8087）。

---

## 6. 接入 deepagents-middleware（二选一）

### 6.1 方式 A：直接部署（显式中间件）

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

### 6.2 方式 B：插件部署（零侵入，推荐）

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

## 7. 生成看板

```bash
./actrail-kv/target/release/actrail-kv-analyze \
  --input requests.ndjson \
  --output analysis.json \
  --top-k 20

./actrail-kv/target/release/actrail-kv-report \
  --input analysis.json \
  --output report.html
```

浏览器打开 `report.html`：

- **命中率表**：每行一次请求，含 prompt / completion / TTFT / 总耗时 列；未上报值的行显示 `—`
- **结构区**：上下文结构代理、模板、缺陷与调优建议（需 ≥3 条相似请求才出模板/缺陷）

---

## 8. 指标字段说明（落盘 `payload._actrail_response_usage`）

| 字段 | 含义 | 来源 |
|---|---|---|
| `prompt_tokens` | 本次请求完整输入 token 数（命中率分母） | 引擎流末 usage（别名 `input_tokens`） |
| `completion_tokens` | 本次输出 token 数 | 引擎流末 usage（别名 `output_tokens`） |
| `ttft_ms` | 首 token 时延（请求发出→首 chunk 到达） | middleware 侧 perf_counter |
| `total_ms` | 总耗时（请求发出→流式结束） | middleware 侧 perf_counter |

引擎未上报 usage 时省略 token 字段；计时始终统计（不依赖引擎开关）。

---

## 9. 一键端到端复测

仓库外自带复测脚本 `ttft_total_e2e_verify.py`（自建带延迟 mock LLM + receiver + daemon，跑全链路并断言；脚本全文见**附录 A**）：

```bash
python ttft_total_e2e_verify.py
# 若二进制不在默认检出位置，可指定代码检出根目录：
# AKV_ROOT=/path/to/checkout python ttft_total_e2e_verify.py
# 需要在容器内运行示例（已装 middleware 依赖的 python）：
# docker exec <container> bash -c 'cd <checkout>/actrail && python ttft_total_e2e_verify.py'
```

通过标志：

```
receiver 落盘 2 条
  ttft_ms/total_ms 真实统计且在预期窗口内，ttft < total ✓
  analysis 透传 ttft_ms/total_ms 与 middleware 统计一致 ✓
  report.html 命中率表 TTFT/总耗时 列渲染实际统计值 ✓
```

---

## 10. 常见问题

1. **看板 TTFT/总耗时 全是 `—`**：capture 未携带 `_actrail_response_usage`。检查 middleware 是否以 `actrail_enabled=True` 接入、`request.model` 是否为标准 `ChatOpenAI`。
2. **命中率恒为 0（reported）**：`cached_tokens` 尚未由 middleware 补传（引擎缓存命中 token）。当前 estimate 口径看 LCP 估算命中率。
3. **mock 验证计时不反映延迟**：mock 服务器必须用 HTTP/1.1 `Transfer-Encoding: chunked`（不能设 `Content-Length`，否则客户端整包读取，ttft≈total）。
4. **daemon 收不到事件**：`daemon.toml` 中 `actrail_receiver_url` 端口须与 receiver `--listen` 一致。
5. **receiver 落盘为空**：relay 为 fire-and-forget，若 daemon 未起或未配 actrail 项则静默丢弃，先看 daemon 日志。

---

## 附录 A：复测脚本全文（ttft_total_e2e_verify.py）

保存为 `ttft_total_e2e_verify.py` 后直接运行（脚本内路径可按本机部署调整 `OUT_DIR` / 各二进制路径）：

```python
"""端到端验证 middleware 流式计时 (ttft_ms / total_ms) 统计并传送到 actrail-kv 看板。

链路：真实 middleware (RamAKvMiddleware.wrap_model_call) 替换 request.model 为
      _KvResponseChatOpenAI -> 其 _generate 走 _stream 聚合真实流式响应时计时
      -> capture.payload._actrail_response_usage 携带 ttft_ms/total_ms
      -> daemon relay -> actrail-kv receiver 落盘 -> analyze -> report.html。

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
RECEIVER = os.path.join(AKV_ROOT, "actrail-kv/target/release/actrail-kv-receiver")
ANALYZE = os.path.join(AKV_ROOT, "actrail-kv/target/release/actrail-kv-analyze")
REPORT = os.path.join(AKV_ROOT, "actrail-kv/target/release/actrail-kv-report")
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
    ndjson_path = os.path.join(OUT_DIR, "requests.ndjson")
    analysis_path = os.path.join(OUT_DIR, "analysis.json")
    report_path = os.path.join(OUT_DIR, "report.html")
    for path in (ndjson_path, analysis_path, report_path):
        if os.path.exists(path):
            os.remove(path)

    # 1) mock LLM（人为流式延迟）
    llm_port = free_port()
    llm_server = ThreadingHTTPServer(("127.0.0.1", llm_port), MockLLMHandler)
    threading.Thread(target=llm_server.serve_forever, daemon=True).start()

    # 2) actrail-kv receiver
    receiver_port = free_port()
    receiver = subprocess.Popen(
        [RECEIVER, "--listen", f"127.0.0.1:{receiver_port}", "--output", ndjson_path],
        stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT,
    )

    # 3) RAM-A daemon relay
    daemon_port = free_port()
    config = os.path.join(OUT_DIR, "daemon.toml")
    with open(config, "w", encoding="utf-8") as fh:
        fh.write("\n".join((
            f'listen_addr = "127.0.0.1:{daemon_port}"',
            "noop_backend = true",
            f'session_store_path = "{OUT_DIR}/sessions.db"',
            f'actrail_receiver_url = "http://127.0.0.1:{receiver_port}"',
            'actrail_model_deployment_key = "deepseek-v4-flash"',
            'actrail_kv_namespace = "default"',
            "",
        )))
    env = dict(os.environ)
    env["RAM_A_KV_CONFIG"] = config
    daemon = subprocess.Popen([DAEMON], env=env,
                              stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)

    try:
        # 4) 等 daemon 健康
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

        # 5) 真实 middleware + 真实 ChatOpenAI（指向 mock LLM）
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

        # 6) 等 receiver 落盘（relay 为 fire-and-forget）
        deadline = time.monotonic() + 8
        lines = []
        while True:
            lines = [line for line in open(ndjson_path, encoding="utf-8") if line.strip()]
            if len(lines) >= 2 or time.monotonic() > deadline:
                break
            time.sleep(0.1)
        print(f"receiver 落盘 {len(lines)} 条")
        if not lines:
            raise RuntimeError("receiver 未落盘任何记录")

        records = [json.loads(line) for line in lines]
        for idx, rec in enumerate(records, 1):
            usage = rec["payload"].get("_actrail_response_usage", {})
            print(f"  记录{idx}: _actrail_response_usage = {json.dumps(usage, sort_keys=True)}")
            assert usage, f"记录{idx} 缺少 _actrail_response_usage"
            assert usage["prompt_tokens"] == 123 and usage["completion_tokens"] == 2, usage
            assert TTFT_LO <= usage["ttft_ms"] <= TTFT_HI, usage
            assert TOTAL_LO <= usage["total_ms"] <= TOTAL_HI, usage
            assert usage["ttft_ms"] < usage["total_ms"], usage
        print("  ttft_ms/total_ms 真实统计且在预期窗口内（300~1500 / 1000~4000），ttft < total ✓")

        # 7) analyze -> report
        subprocess.run([ANALYZE, "--input", ndjson_path, "--output", analysis_path, "--top-k", "20"],
                       check=True, capture_output=True)
        analysis = json.load(open(analysis_path, encoding="utf-8"))
        metrics = analysis.get("cache_metrics", [])
        print("=== analysis.cache_metrics:")
        for m in metrics:
            print("  ", json.dumps(m, ensure_ascii=False, sort_keys=True))
        rec1_usage = records[0]["payload"]["_actrail_response_usage"]
        rec1_metrics = next(m for m in metrics if m.get("basis") == "reported")
        assert rec1_metrics["ttft_ms"] == rec1_usage["ttft_ms"], rec1_metrics
        assert rec1_metrics["total_ms"] == rec1_usage["total_ms"], rec1_metrics
        print("  analysis 透传 ttft_ms/total_ms 与 middleware 统计一致 ✓")

        subprocess.run([REPORT, "--input", analysis_path, "--output", report_path],
                       check=True, capture_output=True)
        html = open(report_path, encoding="utf-8").read()
        tds = set()
        for pat in (str(usage["ttft_ms"]) + " ms", str(usage["total_ms"]) + " ms"):
            assert pat in html, f"report.html 缺少 {pat}"
            tds.add(pat)
        assert html.count("TTFT") >= 1 and "总耗时" in html
        print(f"  report.html 命中率表 TTFT/总耗时 列渲染实际统计值 {sorted(tds)} ✓")
        return 0
    finally:
        llm_server.shutdown()
        for proc in (daemon, receiver):
            if proc.poll() is None:
                proc.terminate()
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.kill()


if __name__ == "__main__":
    raise SystemExit(main())
```
