# 使用说明

## 快速上手

启动 daemon（确保 LMCache-Ascend 后端已在 `lmcache_url` 配置的地址运行）：

```bash
RAM_A_KV_CONFIG=/etc/ram-a-kv/config.toml ram-a-kv
```

另开终端，走完一个典型的会话往返：

```bash
# ram-a-kv daemon 地址（对应配置项 listen_addr）
URL=http://127.0.0.1:6998/event

# 健康检查
curl -s -X POST $URL -H 'Content-Type: application/json' \
  -d '{"type":"health"}' | jq

# 新会话 turn_start（空 map，prefetch_count=0）
curl -s -X POST $URL -H 'Content-Type: application/json' \
  -d '{"type":"turn_start","session_id":"s1"}' | jq

# turn_end 上报本轮 chunk_hashes
curl -s -X POST $URL -H 'Content-Type: application/json' \
  -d '{"type":"turn_end","session_id":"s1","kv_transfer_params":{"chunk_hashes":["h-a","h-b","h-c"]}}' | jq

# 查询当前 map
curl -s -X POST $URL -H 'Content-Type: application/json' \
  -d '{"type":"session_map","session_id":"s1"}' | jq
# -> data.chunk_hashes = ["h-a","h-b","h-c"]

# 关闭会话（释放引用、驱逐 refcount=0 的块）
curl -s -X POST $URL -H 'Content-Type: application/json' \
  -d '{"type":"session_close","session_id":"s1"}' | jq
```

## HTTP API

daemon 的唯一路由为 `POST /event`。请求体为 JSON，必须包含 `type` 字段。响应统一格式 `EventResponse`：

```json
{ "ok": true,  "type": "turn_end", "data": { ... } }
{ "ok": false, "type": "turn_end", "error": "..." }
```

### 元事件 list_events

`POST /event` 携带 `{"type":"list_events"}` 返回所有已注册事件及其 `description` / `required` / `optional`，便于客户端发现契约：

```bash
curl -s -X POST http://127.0.0.1:6998/event \
  -H 'Content-Type: application/json' \
  -d '{"type":"list_events"}' | jq
```

## 事件清单

共 9 个事件。下表列出每个事件的 `type`、必填 / 可选字段、成功响应 `data`。

| type | 必填 | 可选 | 成功响应 data |
|---|---|---|---|
| `turn_start` | `session_id` | — | `{prefetch_sent, prefetch_count, backend_degraded}` |
| `turn_end` | `session_id`, `kv_transfer_params` | `debug_context` | `{evicted_count, map_updated, debug_written, backend_degraded, chunk_count}` |
| `snapshot_restore` | `session_id` | — | `{prefetch_sent, prefetch_count, backend_degraded, evicted_count, pinned}` |
| `session_map` | `session_id` | — | `{session_id, chunk_hashes}` |
| `session_close` | `session_id` | — | `{closed, pinned, evicted_count, backend_degraded}` |
| `session_suspend` | `session_id` | — | `{suspended, pinned, evicted_count, backend_degraded}` |
| `session_fork` | `session_id` | — | `{forked, fork_id}` |
| `session_fork_end` | `session_id` | `fork_id` | `{fork_end, evicted_count, backend_degraded, matched}` |
| `health` | — | — | `{status:"running", sessions_count}` |

### 事件使用场景

- **`turn_start`**：每一轮推理开始时上报。daemon 根据会话当前 chunk map 向后端发起 prefetch，将所需 KV cache 块预载入加速卡，降低本轮推理的 TTFT。
- **`turn_end`**：每一轮推理结束后上报，携带本轮实际产生的 `chunk_hashes`。daemon 据此更新会话的 chunk map，对新旧引用做差集计算，驱逐不再被任何会话引用的块。
- **`snapshot_restore`**：会话从挂起状态恢复或 daemon 重启后恢复时上报。daemon 从内存或 SQLite 加载该会话的 chunk map，重新注册引用并发起 prefetch，使会话状态得以延续。
- **`session_map`**：查询某个会话当前引用的 chunk_hashes 列表，用于排查与观测会话状态。
- **`session_close`**：会话永久结束时上报。daemon 释放该会话对所有 chunk 的引用，驱逐 refcount 降为 0 的块，并从 SQLite 删除该会话记录。
- **`session_suspend`**：会话临时挂起时上报（如用户暂停对话）。daemon 释放内存中的引用并驱逐相应块，但**保留** SQLite 记录，便于后续通过 `snapshot_restore` 恢复。
- **`session_fork`**：会话分叉时上报（如从已有会话派生新会话）。daemon 对当前会话所有 chunk_hashes 的 refcount +1，防止父会话后续 `turn_end` 误驱逐子会话仍需的块。
- **`session_fork_end`**：分叉结束（子会话已独立运行）时上报。daemon 对称地 refcount -1，并驱逐 refcount 降为 0 的块。
- **`health`**：健康检查，探测 daemon 是否正常运行并返回当前活跃会话数，用于监控与就绪检测。
