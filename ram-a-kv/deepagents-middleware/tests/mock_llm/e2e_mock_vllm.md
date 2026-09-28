# 端到端与交付回归测试

统一入口验证真实 DeepAgents → mock vLLM → Python SDK → 真实 RAM-A daemon → SQLite。模型服务和缓存后端为 mock / noop；不验证实际 GPU KV 数据搬运、命中率或性能。

## 运行

先按[使用指南](../USE_GUIDE.md)安装依赖，并构建 RAM-A daemon（在 `ram-a-kv` 目录执行 `cargo build`）。当前版本 daemon 已默认支持 `agent_event` 事件。

```bash
export RAM_A_KV_DAEMON_BIN=/absolute/path/to/RAM-A/ram-a-kv/target/debug/ram-a-kv
PYTHON_BIN=python bash tests/run_e2e_mock_vllm.sh
PYTHON_BIN=python bash tests/run_e2e_mock_vllm.sh --async
```

`--daemon-bin` 可覆盖环境变量；`--output-dir` 指定尚不存在的日志目录，默认创建 `/tmp/ram-a-e2e-*`。脚本启动并回收临时服务，产物保留。需允许本地端口监听；受限沙箱中的线程和异步调度也可能受环境限制。

输出包含回归检查、handler 检查、六条主链路证据以及 `RESULT: PASS`。测试失败退出 1，参数不完整退出 2。故意注入失败的回归检查会产生 warning，这些 warning 不表示主链路失败。

## 字段来源

- 测试侧 `AgentRequestIds` 模拟应用已有的请求 ID 生产者，先把 `test-agent-request-1/2` 放入模型请求，再进入 RAM-A Middleware。此类只存在于测试中。
- mock vLLM 生成独立 completion ID，返回 `ucm_block_ids` 与 `prefech_rank: 3`。它也接受不带 request_id 的请求。
- 第一轮返回 1～28、第二轮返回 2～29 的 32-hex 句柄；两轮重合 27 个。生产代码不生成这些句柄。

## 验收检查

| 范围 | 检查 |
| --- | --- |
| 原始 ID | 已有 request_id 原样传递；无 ID 不制造值；原模型和请求配置不被修改 |
| 会话 | 缺失 ID 不发送事件；session_id=0 正常；不同并发会话 ID 隔离 |
| KV | 缺失、空、错误类型、过长、过多、非法 Unicode 均不打断模型；有效值映射到 chunk_hashes |
| 故障 | provider / HTTP 失败保留回复；模型异常传播；错误日志带关联信息；业务错误码明确 |
| 资源 | 同步/异步严格 transport 可多次复用，SDK 关闭时不关闭借用的 transport |
| 数据边界 | 默认不发送 state、上下文凭据或请求头；调试合并大小限制和脱敏 |
| graph | 一次执行中工具循环产生两次模型事件；两个会话并发异步执行不串 ID |
| ACK | 相同 event_id 重试不变；畸形响应触发有限重试；真实 daemon 重复事件返回 duplicate |
| handler | 空 session、空/超长 event_id、非对象 payload 被拒绝 |
| 主链路 | 两次真实模型 HTTP 请求，每次 agent/start → turn_start → turn_end；六次 ACK 与 daemon 日志一致 |
| 预取与替换 | 第一轮预取 0；第二轮预取 28；替换映射后释放 1 个旧引用，session_map 读回一致 |

服务端容量与 TTL 使用 RAM-A 的 Rust 单测验证，在 ram-a-kv 目录运行 `cargo test --locked --workspace`。主链路日志提取只筛选 `deepagents:mock-vllm-e2e`；额外 handler 探针使用 `contract-only`，不写入 KV 映射。

## 产物与范围

| 产物 | 内容 |
| --- | --- |
| ram-a.log | 原始 handler 结果、关联信息与 manager trace |
| mock-vllm.jsonl | 模型请求和原始回复 |
| events.json | 主链路 SDK 请求与 ACK |
| evidence.json | 校验通过后提取的六条主链路事件 |
| ram-a.toml / sessions.db | 临时配置与数据库 |

模型请求 ID 对应 SDK model_request_id；completion ID 单独保留在响应 metadata，不混为同一字段。rank 仅为 metadata，不参与后端选择。

脚本验证源码路径，不替代 wheel 安装验收。发行时还需构建 wheel、检查版本、从安装包运行使用指南示例。真实 vLLM 集成、流式、Responses API、同一会话的并发写入、长时间故障恢复、数据库重启恢复不属于本套完整验证范围。应用应串行更新同一会话的缓存映射。
