# ram-a-kv 使用指南

## 1. 简介

ram-a-kv 是一个 **KV 缓存（KV cache）协调守护进程（daemon）**，用于在多轮推理（multi-turn inference）场景下管理“会话级” KV cache 块（chunk）的生命周期：

- 哪些 chunk 需要预取（prefetch）到加速卡；
- 哪些 chunk 可以驱逐（evict）；
- 跨会话的引用计数（refcount），保证被多个会话共享的 chunk 不会被某个会话提前删除。

## 2. 架构概览

| 组件 | 说明 |
|---|---|
| **ram-a-kv（daemon）** | HTTP 服务、事件分发、SQLite 持久化、启动恢复。二进制名 `ram-a-kv`。 |
| **manager-core** | 纯业务逻辑库：会话状态、引用计数、驱逐策略、后端 trait。无 HTTP / SQLite / CLI。 |
| **backend** | `NoopBackend` / `LMCacheAscendBackend`，实现 `KvCacheBackend`（prefetch / evict / query）。 |
| **ram-a-kv-sdk** | 给宿主应用（如 xiaoo）用的客户端库，封装事件上报。 |
| **openclaw-plugin** | TypeScript 插件，通过 SDK 把推理事件桥接到 ram-a-kv。 |

## 3. 构建

`ram-a-kv/` 本身是一个 Cargo workspace：

```bash
cd ram-a-kv
cargo build --release
```

产物：`ram-a-kv/target/release/ram-a-kv`（即可执行 daemon 二进制）。

## 4. 配置

daemon 通过环境变量 `RAM_A_KV_CONFIG` 指定配置文件路径；未设置时默认读取 `/etc/ram-a-kv/config.toml`；文件不存在或解析失败时回退到内置默认值。

配置项（TOML 顶层字段）：

| 字段 | 默认值 | 说明 |
|---|---|---|
| `listen_addr` | `127.0.0.1:6998` | HTTP 监听地址 |
| `lmcache_url` | `http://localhost:6999` | LMCache-Ascend 后端地址 |
| `debug_enabled` | `false` | 是否写 debug 文件 |
| `debug_dir` | `kvcache_debug` | debug 文件输出目录 |

最小示例（参考 `config.example.toml`）：

```toml
listen_addr = "127.0.0.1:6998"
lmcache_url = "http://localhost:6999"
debug_enabled = false
debug_dir = "kvcache_debug"
```

## 5. 启动

```bash
RAM_A_KV_CONFIG=/path/to/config.toml ./target/release/ram-a-kv
```

日志默认输出到 stdout，关键日志：

- `ram-a-kv daemon starting`（启动，带 addr 与 noop 标志）
- `ram-a-kv event received`（收到事件，带 type / session_id）
- `ram-a-kv event completed` / `ram-a-kv event failed`（处理结果）

## 6. HTTP API

唯一的路由：`POST /event`。请求体为 JSON，必须包含 `type` 字段。

响应统一格式 `EventResponse`：

```json
{ "ok": true,  "type": "turn_end", "data": { ... } }
{ "ok": false, "type": "turn_end", "error": "..." }
```

`type` 字段在失败时可能缺省（如缺 `type` 字段的请求）。`data` / `error` 仅在对应情形下出现。

### 元事件 list_events

`POST /event` 携带 `{"type":"list_events"}` 返回所有已注册事件及其 `description / required / optional`，便于发现契约：

```bash
curl -s -X POST http://127.0.0.1:6998/event \
  -H 'Content-Type: application/json' \
  -d '{"type":"list_events"}' | jq
```

### 字段校验

- 请求体缺 `type` 或 `type` 为空字符串 → 拒绝（`missing or empty 'type' field`）。
- `type` 不在已注册事件中 → 拒绝（`unknown event type`）。
- handler 声明的 `required` 字段缺失 → 拒绝（`missing required field '<field>'`）。
- 各业务事件的 `session_id` 为空字符串 → 拒绝（`session_id must not be empty`）。

## 7. 事件清单

共 9 个事件。下表列出每个事件的 `type`、必填 / 可选字段、成功响应 `data`。

| type | 必填 | 可选 | 成功响应 data |
|---|---|---|---|
| `turn_start` | `session_id` | — | `{prefetch_sent, prefetch_count}` |
| `turn_end` | `session_id`, `kv_transfer_params` | `debug_context` | `{evicted_count, map_updated, debug_written}` |
| `snapshot_restore` | `session_id` | — | `{prefetch_sent, prefetch_count}` |
| `session_map` | `session_id` | — | `{session_id, chunk_hashes}` |
| `session_close` | `session_id` | — | `{evicted_count, closed}` |
| `session_suspend` | `session_id` | — | `{evicted_count, suspended}` |
| `session_fork` | `session_id` | — | `{forked}` |
| `session_fork_end` | `session_id` | — | `{fork_end}` |
| `health` | — | — | `{status:"running", sessions_count}` |

### 7.1 典型往返（curl）

```bash
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

### 7.2 turn_end 的 debug_context

`turn_end` 可选携带 `debug_context`，包含推理消息与计时，daemon 会在 `debug_enabled=true` 时落盘 debug 文件：

```json
{
  "type": "turn_end",
  "session_id": "s1",
  "kv_transfer_params": { "chunk_hashes": ["h1", "h2"] },
  "debug_context": {
    "messages": [ {"role": "user", "content": "hi"} ],
    "timing": { "ttft_ms": 120, "total_time_ms": 450, "tpot_ms": 30 }
  }
}
```

`timing` 各字段均可选；`messages` 为空数组也可。只要 `messages` 或 `timing` 至少一个非空，就会生成 debug 文件。

### 7.3 snapshot_restore

`snapshot_restore` 先查内存，内存没有则从 SQLite 加载该 session 的 map，取 `chunk_hashes` 后向 manager 注册引用并发 prefetch。适合在 daemon 重启后、或 suspend 后恢复时调用。

## 8. 引用计数与驱逐语义（重要）

ram-a-kv 的核心安全不变式：

> **一个 chunk 只有在全局引用计数（refcount）降为 0 时才会被驱逐**，因此被多个会话共享的 chunk 绝不会被某个会话提前删掉。

- **`turn_end`**：用新 `chunk_hashes` 与旧集合做差集——不再引用的 refcount -1、新引用的 refcount +1；refcount 到 0 的进入驱逐列表并从计数表移除，向后端发 evict。
- **`session_fork`**：对当前会话所有 chunk_hashes refcount +1，防止父会话后续 `turn_end` 误驱逐子会话仍需的块；`session_fork_end` 对称地 refcount -1 并驱逐 refcount=0 的块。
- **`snapshot_restore`**：按唯一 hash 计数（局部去重，避免重复输入导致多计），并按客户端传入的**原始顺序**透传给后端 prefetch，保持与手动 curl 一致的顺序。

## 9. 持久化与重启恢复

- 持久化用 SQLite，schema：
  ```sql
  CREATE TABLE sessions (
    session_id   TEXT PRIMARY KEY,
    map_json     TEXT NOT NULL,
    turn_count   INTEGER NOT NULL DEFAULT 0
  );
  ```
- **`turn_end` 成功后**：将当前 session 的 map 写入 SQLite（`INSERT OR REPLACE`）。
- **`session_close`**：从 SQLite 删除该 session。
- **`session_suspend`**：释放引用并驱逐内存中的块，但**保留** SQLite 记录，便于后续 `snapshot_restore` 恢复。
- **daemon 启动时**：遍历 SQLite 所有 session，全部 `restore_session` 回内存，使状态在重启后存活。
- `snapshot_restore`：先查内存，内存没有则从 SQLite 加载，再注册引用并发 prefetch。

## 10. SDK 用法（ram-a-kv-sdk）

给宿主应用集成的客户端库。它一次性从配置初始化全局客户端（`OnceLock`），之后所有事件都是 **fire-and-forget**（异步 spawn，不阻塞调用方、不返回结果）。

配置文件路径默认 `~/.config/ram-a-kv/config.toml`，内容仅一项：

```toml
daemon_url = "http://localhost:6998"
```

Rust 用法：

```rust
use ram_a_kv_sdk::RamAKvClient;

// 进程启动时初始化（读默认配置文件）
RamAKvClient::init_from_config();
// 或指定路径：RamAKvClient::init_from_path(path.as_path());

assert!(RamAKvClient::is_enabled());

RamAKvClient::turn_start("s1");
RamAKvClient::turn_end(
    "s1",
    Some(&kv_transfer_params),   // 含 chunk_hashes 的 Value
    Some(&debug_context),         // 可选
);
RamAKvClient::snapshot_restore("s1");
RamAKvClient::session_fork("s1");
RamAKvClient::session_fork_end("s1");
RamAKvClient::session_suspend("s1");
RamAKvClient::session_close("s1");
```

> 注意：`OnceLock` 初始化后不可重载，改了 SDK 配置必须**重启宿主进程**。

## 11. 与 xiaoo 集成

1. 启动 ram-a-kv daemon（测试建议 `noop_backend = true`）。
2. 写 SDK 配置 `~/.config/ram-a-kv/config.toml`（`daemon_url`）。**注意：daemon 地址只能通过 SDK 自己的配置文件指定**，xiaoo 的 LlmConfig 没有 `kvcache_daemon_url` 字段。
3. 在 xiaoo 配置里开启：`kvcache_enabled = true`（`[llm]` 段）。可选 `kvcache_debug_enabled = true`。
4. 启动 xiaoo TUI（改 SDK 配置后必须重启，因 OnceLock 不可重载）。
5. 验证：daemon stdout 应出现
   `type=turn_start ... ram-a-kv event completed` 与
   `type=turn_end ... ram-a-kv event completed`。

完整步骤也可运行 `./tests/test_integration.sh xiaoo` 查看联调指南。

## 12. openclaw-plugin 集成

`openclaw-plugin/` 是 TypeScript 插件，通过 SDK 把推理事件桥接到 ram-a-kv。部署方式详见 [`openclaw-plugin-deploy.md`](./openclaw-plugin-deploy.md)。

## 13. 调试与排查

- **debug 文件**：`debug_enabled = true` 且 `turn_end` 携带 `debug_context` 时，会在 `debug_dir`（默认 `kvcache_debug`）写出 `kvcache_debug_<session_id>_<turn>.json`，含消息与计时。
- **trace 快照**：`trace_events = true` 时，每个事件后打印所有 session 的 chunk_hashes 与全局 refcounts，便于观察状态变化。
- **后端往返日志**：LMCache backend 的 prefetch / evict / query 会打印 status、count、response。
- **LMCache 后端端点**（生产模式）：daemon 会向 `{lmcache_url}` 发：
  - `POST /memory/prefetch`，body `{"chunk_hashes": [...], "lookup_id": "..."}`；
  - `POST /memory/evict`，body `{"chunk_hashes": [...]}`；
  - `POST /memory/query`，body `{"chunk_hashes": [...]}`，返回 `hash -> [locations]`。
  - 预取 / 驱逐超时均为 5s。
- 常见问题：
  - `daemon did not start (port 6998 not listening)`：检查 `listen_addr` 是否被占用。
  - 事件被拒 `missing or empty 'type' field`：请求体缺 `type`。
  - 事件被拒 `unknown event type`：`type` 不在 9 个已注册事件里。
  - 事件被拒 `missing required field '...'`：缺必填字段（如 `turn_end` 缺 `kv_transfer_params`）。
  - `session '<id>' not found`：`session_map` 查了不存在的会话。

## 14. 测试

`tests/test_integration.sh` 提供两种模式：

```bash
./tests/test_integration.sh          # 跑 L1 daemon 契约测试（noop_backend，无需 LMCache）
./tests/test_integration.sh xiaoo    # 打印 xiaoo 联调步骤指南
```

脚本会先 `cargo build --release` 出 daemon，再起 daemon、发事件、断言 `ok` / 字段、验证持久化与重启恢复。支持环境变量：`DAEMON_BIN` / `TEST_DIR` / `DAEMON_URL` / `KEEP_DAEMON`。
