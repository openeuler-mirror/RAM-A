# Real-LLM 端到端验证

针对真实模型服务(OpenAI 兼容 `/v1/chat/completions`)验证
DeepAgents + RamAKvMiddleware -> RAM-A daemon 链路。

## 运行

```bash
# 前置:仓库根目录已有 .venv(pip install '.[deepagents]'),
# 且 ram-a-kv daemon、模型服务已启动
bash tests/real_llm/run_verify_real.sh

# 覆盖默认地址
MODEL_BASE_URL=http://127.0.0.1:8000/v1 \
RAM_A_URL=http://127.0.0.1:6990 \
VENV_PY=/path/to/python \
bash tests/real_llm/run_verify_real.sh
```

通过标准:

1. 输出 `[setup] model=...` 正确探测到模型名
2. 两轮 `[turn] user=... assistant=...` 均有真实模型回复
3. 输出 `AGENT_RUN_OK`
4. 末尾 `session_map` 返回该会话的 chunk_hashes 映射
   (前提:模型响应携带 `kv_transfer_params`;否则 turn_end 以
   `chunk_hashes: null` 发送,见下方日志说明)

## Traffic 日志(现场验证用)

运行脚本会自动开启 traffic 日志,默认写到**脚本同目录**的
`ram-a-traffic.log`(每次运行清空重写)。每条记录为单行 JSON,
由 `src/ram_a_middleware/sdk/traffic_log.py` 输出。

### 环境变量

| 变量 | 说明 |
| --- | --- |
| `RAM_A_TRAFFIC_LOG` | 日志文件绝对路径;**未设置时默认脚本同目录**;设为空值则完全关闭(零开销) |
| `RAM_A_TRAFFIC_MAX_BYTES` | 单条记录字节上限,超出自动截断为 `{"truncated": true, "bytes": ..., "preview": ...}`(默认 65536) |
| `RAM_A_TRAFFIC_STDERR` | 设为 `1` 时同时镜像到 stderr |

日志内部所有异常均被吞掉,任何日志故障都不会影响事件投递。

### 顶层公共字段(所有记录)

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `ts` | string | 本地时间戳,格式 `YYYY-MM-DDTHH:MM:SS±ZZZZ` |
| `dir` | string | **渠道标识,区分日志来源的核心字段**,取值见下表 |
| `type` | string | 事件类型;`request` 记录中位于 `body` 内,其余记录在顶层 |

### `dir` 渠道一览

| `dir` 值 | 渠道 | 产生位置 | 记录内容 |
| --- | --- | --- | --- |
| `request` | middleware -> ram-a 请求 | `sdk/protocol.py` `_post()` | 发给 daemon 的完整 POST body |
| `response` | ram-a -> middleware 应答 | 同上,收到 ACK 后 | daemon 对该事件的确认结果 |
| `deepagents` | deepagents 侧事件 | `deepagents/middleware.py` `wrap_model_call` 前后 | 模型调用请求体与响应摘要 |

### `type` 事件类型一览

| `type` 值 | 出现的 `dir` | 触发时机 | 说明 |
| --- | --- | --- | --- |
| `agent_event` | request / response | 每次 agent invoke 启动时(`before_agent` 阶段) | `event_name=agent`、`event_action=start`;带 `retries` 重试,每次尝试各记一条 response |
| `turn_start` | request / response | 每次模型调用前(`wrap_model_call` 内、handler 执行前) | 若 daemon 侧开启 `turn_start_prefetch`,daemon 会在此时预取 |
| `turn_end` | request / response | 每次模型调用后 | 携带 `kv_transfer_params.chunk_hashes`;模型无 KV 字段时为 `null` |
| `model_request` | deepagents | 模型调用前(turn_start 发送之前) | 发给模型服务的请求体摘要 |
| `model_response` | deepagents | 模型调用后(turn_end 发送之前) | 模型响应摘要,含 `has_kv_transfer_params` 标记 |

### 各渠道记录的完整字段

**1. `dir=request`(middleware -> ram-a 请求)**

| 字段 | 说明 |
| --- | --- |
| `url` | ram-a daemon 的 `/event` 端点地址 |
| `body` | 完整 POST body,含 `type`、`event_id`、`session_id`、`agent_turn_id`,以及按事件类型的专属字段(见下方) |

`body` 按事件类型的专属字段:

- `agent_event`:`event_name`、`event_action`、`source`("deepagents")、`payload`(`message_count`)、`detail_level`、`schema_version`
- `turn_start`:`model`(模型名)、`model_request_id`(若模型请求携带)
- `turn_end`:`model`、`model_request_id`、`kv_transfer_params`(`chunk_hashes` 数组或 `null`)、`debug_context`(`timing.total_time_ms`、`response_metadata`)

**2. `dir=response`(ram-a -> middleware 应答)**

| 字段 | 说明 |
| --- | --- |
| `type` | 事件类型(与请求对应) |
| `event_id` | 与请求的 `event_id` 一致,用于请求/应答对账 |
| `ok` | daemon 是否接受 |
| `status` | `success` / `degraded`(事件已接收但缓存后端降级,如 LMCache 不可达)/ `error` |
| `error_code` | 失败时给出(`http_<code>` / `daemon_error` / 异常类名等) |
| `error` | 失败原因文本 |

**3. `dir=deepagents, type=model_request`(发给模型的请求体)**

| 字段 | 说明 |
| --- | --- |
| `session_id` | 会话标识(`deepagents:<thread_id>`) |
| `model_request_id` | 模型请求携带的 request_id(未携带为 `null`) |
| `model` | 模型名 |
| `model_settings` | 模型调用参数(含 `extra_body`,如 request_id) |
| `messages` | 完整消息列表,每条为 `{"role": ..., "content": ...}`;复杂 content 经 json_safe 序列化 |

**4. `dir=deepagents, type=model_response`(模型响应摘要)**

| 字段 | 说明 |
| --- | --- |
| `session_id` / `model_request_id` | 同上,用于关联 |
| `messages` | 每条响应消息的 `{"role": ..., "id": ..., "has_kv_transfer_params": ...}`;`has_kv_transfer_params=false` 即模型未返回 KV 字段(此时 turn_end 发 null) |

**5. 校验失败未发出的事件**

事件未通过 middleware 侧校验(未发到 daemon)时,记一条
`dir=request`、`body={"type": ..., "validation_error": "<原因>"}` 的记录。

### 关联键

- 同一模型调用轮次:`session_id` + `agent_turn_id` 贯穿 agent_event、
  model_request、turn_start、model_response、turn_end 五类记录
- 请求/应答对账:`event_id`
- request 记录的 `type` 在 `body` 内,response/deepagents 记录的 `type`
  在顶层(过滤时注意)

### 示例(一轮完整调用,按时间序)

```json
{"ts": "...", "dir": "request", "url": "http://127.0.0.1:6990/event", "body": {"event_name": "agent", "event_action": "start", "session_id": "deepagents:verify-real-1", "agent_turn_id": "turn-...", "source": "deepagents", "event_id": "...", "payload": {"message_count": 1}, "detail_level": "summary", "schema_version": 1, "type": "agent_event"}}
{"ts": "...", "dir": "response", "type": "agent_event", "event_id": "...", "ok": true, "status": "success", "error_code": null, "error": null}
{"ts": "...", "dir": "deepagents", "type": "model_request", "session_id": "deepagents:verify-real-1", "model_request_id": null, "model": "deepseek", "model_settings": {}, "messages": [{"role": "human", "content": "你好..."}]}
{"ts": "...", "dir": "request", "url": "http://127.0.0.1:6990/event", "body": {"session_id": "deepagents:verify-real-1", "agent_turn_id": "turn-...", "model": "deepseek", "type": "turn_start", "event_id": "..."}}
{"ts": "...", "dir": "response", "type": "turn_start", "event_id": "...", "ok": true, "status": "success", "error_code": null, "error": null}
{"ts": "...", "dir": "deepagents", "type": "model_response", "session_id": "deepagents:verify-real-1", "model_request_id": null, "messages": [{"role": "ai", "id": "chatcmpl-...", "has_kv_transfer_params": true}]}
{"ts": "...", "dir": "request", "url": "http://127.0.0.1:6990/event", "body": {"session_id": "deepagents:verify-real-1", "agent_turn_id": "turn-...", "model": "deepseek", "kv_transfer_params": {"chunk_hashes": ["a7cf...", ...]}, "debug_context": {"timing": {"total_time_ms": 19}, ...}, "type": "turn_end", "event_id": "..."}}
{"ts": "...", "dir": "response", "type": "turn_end", "event_id": "...", "ok": true, "status": "degraded", "error_code": null, "error": null}
```

### 示例逐条说明(与上方 JSON 按时间序一一对应)

| 序 | dir / type | 这条日志在做什么 |
| --- | --- | --- |
| 1 | `request` / `agent_event` | agent 启动(`before_agent` 钩子)时向 daemon 上报 `agent/start`,宣告一轮 agent 交互开始。`agent_turn_id` 为本轮标识,后续 turn_start/turn_end 复用同一值;`payload.message_count` 是启动时的消息数 |
| 2 | `response` / `agent_event` | daemon 对第 1 条的 ACK。`ok=true, status=success` 即已接受;`event_id` 与请求一致,用于请求/应答对账(agent_event 带 retries 重试,每次尝试各记一条 response,`event_id` 不变) |
| 3 | `deepagents` / `model_request` | 本地旁路记录(**不发给 daemon**):即将发给模型服务的请求摘要,含完整 messages。`model_request_id=null` 表示本次模型请求未携带 request_id,后续 turn_start/turn_end 中同样为 null |
| 4 | `request` / `turn_start` | 模型 handler 执行前向 daemon 上报 turn_start。daemon 侧若开启 `turn_start_prefetch`,会在收到该事件时预取 KV 缓存。与第 3 条同一时机,一条发给 daemon、一条本地留档 |
| 5 | `response` / `turn_start` | daemon 对 turn_start 的 ACK |
| 6 | `deepagents` / `model_response` | 模型返回后的本地旁路记录:响应消息摘要。`has_kv_transfer_params=true` 表示模型响应携带 KV 字段,可从中提取 `ucm_block_ids` 作为 chunk_hashes;若为 `false`,随后 turn_end 将发送 `chunk_hashes: null` |
| 7 | `request` / `turn_end` | 模型调用结束后上报 turn_end:携带 `kv_transfer_params.chunk_hashes`(daemon 据此建立/更新该会话的缓存映射,对应通过标准第 4 条)与 `debug_context.timing.total_time_ms`(本次模型调用耗时) |
| 8 | `response` / `turn_end` | daemon 对 turn_end 的 ACK。示例中 `status=degraded` 表示事件已接收但缓存后端(如 LMCache)降级,非链路故障;若 `ok=false` 则说明事件被拒(如 chunk_hashes 非法) |

整体节奏:第 1-2 条是 agent 生命周期边界;第 3-5 条在模型调用前;
第 6-8 条在模型调用后。`session_id` + `agent_turn_id` 贯穿全部 8 条,
将它们串成同一轮调用;`event_id` 则将每个 request/response 对账。

### 过滤命令

```bash
# 只看 ram-a 链路的请求
grep '"dir": "request"' ram-a-traffic.log

# 只看 ram-a 的应答
grep '"dir": "response"' ram-a-traffic.log

# 只看 deepagents 侧事件
grep '"dir": "deepagents"' ram-a-traffic.log

# 只看某类事件(如 turn_end 的请求与应答)
grep turn_end ram-a-traffic.log

# 按字段精确解析并输出时间线
python3 -c "
import json
for line in open('ram-a-traffic.log'):
    r = json.loads(line)
    print(r['ts'], r['dir'], r.get('type'), r.get('body', {}).get('type', ''))
"
```

## 已知行为

- 模型响应不含 `kv_transfer_params` 时,turn_end 仍会发送,但
  `chunk_hashes` 为 `null`;daemon(未改动侧)会返回错误 ACK
  (`ok=false, error="invalid turn_end payload: ..."`),
  traffic 日志中该请求与应答均可见
- daemon 应答 `status=degraded` 表示事件已接收但缓存后端降级
  (如 LMCache 不可达),非链路故障
- `agent_event` 带 `retries` 重试(默认 0 次),每次尝试的应答
  各记一条,`event_id` 不变
- `model_request` 的 `messages` 包含完整用户提示词,
  现场开启日志时注意敏感内容
