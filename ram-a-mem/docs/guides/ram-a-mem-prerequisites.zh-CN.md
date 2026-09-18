# RAM-A-MEM 使用前置约束

本文说明使用 `ram-a-mem` 前必须由使用方准备的外部条件：模型服务、客户端和网络环境。
安装产物本身（`ram-a-mem` 二进制、交付 JSON 配置模板、RPM 目录结构）不在本文范围。
逐字段的配置约束见
[`ram-a-mem-configuration-reference.zh-CN.md`](ram-a-mem-configuration-reference.zh-CN.md)；
模型协议细节见
[`model-compatibility.zh-CN.md`](model-compatibility.zh-CN.md)。

## 1. 最小可用：只需一个 Chat 模型服务

个人记忆的摄入（Extract/Ground）和检索全部跑通，外部依赖只有一项：

**一个 OpenAI-compatible Chat Completions 端点**，要求：

1. 接受 Bearer 鉴权的 `POST /chat/completions`；
2. `extractor_model` 和 `verifier_model` 填该端点上的实际模型名，两者可用同一个模型；
3. 模型的最终答案稳定返回在 `choices[0].message.content`。只有
   `reasoning_content` 而没有 `content` 的响应会被判定为协议错误，导致本次摄入失败；
4. 模型能稳定输出 JSON（Extract 返回 `atomic_memory_v1`，Ground 返回 `results`）。
   弱 JSON 模型可用 `json_repair_attempts=1` 兜底，但该修复至多一次且不允许新增事实。

该端点的 API Key 通过环境变量注入（`providers.api_key_env` 指向的变量名）。**即使
Embedding 使用本地 hash，该环境变量也必须存在且非空**；免认证的本地端点同样需要
一个非空占位值。

### 推理模型的处理

RAM-A 不要求使用非推理模型，但要求最终答案落在 `content`。推理模型存在"短输出
预算下只生成 reasoning"的风险，两种应对（互斥，不能同时配置）：

- `reasoning_effort: "none"`：适用于接受该字段的服务，例如已验证的 GLM Coding Plan；
- `enable_thinking: false`：仅适用于明确接受该布尔字段的服务。

两者都不支持时，`reasoning_only_retry: true` 可在 content 为空时纠正重试一次，但
不保证服务端真正关闭推理；生产环境应优先选用可关闭推理或推理不影响 content 的模型。

### Embedding：最小配置可不依赖外部服务，实际使用需要

`embedding_provider: "hash"` 在本地确定性计算向量，不调用任何外部服务，可让全链路
（含检索）离线跑通。但 hash 向量没有语义含义，检索只相当于随机排序加 BM25 文本匹配，
仅适合部署自测。

**语义记忆检索需要**一个 OpenAI-compatible `/v1/embeddings` 端点，并提供与模型实际
输出一致的 `embedding_model` 和 `embedding_dimensions`。

### Embedding 模型中途不可更换

RAM-A 为每个 scope 记录 embedding profile。同一 scope 内，查询向量或新写入记忆的
维度/模型与已存 profile 不一致时直接失败，不混用不同向量空间——两个 Provider 即使
维度相同、语义空间不同也会被拒绝。因此更换 Embedding 模型意味着使用新的数据库文件
（或清空旧库），不存在"新旧向量平滑过渡"的路径。

## 2. 按功能叠加的模型服务

以下功能关闭时不需要任何额外模型；开启后各自有必配项：

| 开启功能 | 额外必须准备 | 说明 |
| --- | --- | --- |
| Rerank | 一个兼容 OpenRouter 协议的 `POST /rerank` 端点 | 仅 `retrieval.mode=hybrid` 时可启用；自托管需按该协议实现，`api_key_env=null` 表示免认证本地端点 |
| Graph Memory | 一套独立的图 LLM 配置（`llm_api_key_env`、`llm_base_url`、`llm_model`） | 不继承主 `providers` 的任何兼容性字段和 `max_retries`，重试固定最多 5 次 |
| 案例库 | 案例检索 Embedding（可 hash） | 模型摘要可选，`summary_llm_model=null` 表示不启用模型摘要 |

Graph Memory 的图 LLM 与主 Chat 模型可以相同也可以不同，但配置完全独立，需要单独
提供 key 和端点。

## 3. MCP 客户端要求

调用 `ram-a-mem` 的客户端（xiaoO 或其他）必须：

1. 支持 Streamable HTTP 传输，连接 `POST /mcp`；
2. 协商 MCP 协议版本 `2025-11-25`；
3. 每个请求携带 `Authorization: Bearer <token>`，Token 与服务端 `auth.tokens` 中
   某项的环境变量值一致；
4. 发送 `X-Agent-ID` 时（可选），值必须与该 Token 配置的 `agent_id` 一致；
5. 不把 `Authorization`、`Origin`、`X-Agent-ID`、`mcp-session-id`、
   `mcp-protocol-version` 写入静态 headers——这些由客户端按会话动态管理。

记忆抽取是同步 LLM 调用，单次 `memory_ingest` 的耗时取决于 Chat 模型响应速度；
客户端 `timeout_ms` 应留足余量（xiaoO 示例为 30000ms）。

## 4. 网络与部署边界

- 默认监听 loopback。**对外（非 loopback）暴露必须满足三个条件**：服务前已真实部署
  TLS termination、`allowed_hosts` 配置实际访问的 Host（含端口）、
  `tls_termination_acknowledged=true`。该字段只是部署者的确认声明，RAM-A 自身不会
  因此启用 TLS——没有 TLS termination 的裸 HTTP 对外监听是不被支持的部署方式。
- 带 API Key 的公网 Provider URL 固定要求 HTTPS；loopback、私网和 link-local 地址
  允许 HTTP。URL 不允许携带用户名、密码、query 或 fragment。
- 开启 Graph Memory 时，图构建在 `memory_ingest` 响应前同步完成。RAM-A 前面的反向
  代理或负载均衡器的请求读超时必须大于图 LLM 超时加其重试退避总和，SSE keep-alive
  不能替代该超时要求。
- 超过速率或并发限制的请求直接返回 HTTP 429（带 `Retry-After: 1`），不排队等待。
- 个人记忆 SQLite、案例库业务库、案例检索索引库是三个必须分开的文件；个人记忆库
  应只由一个 `ram-a-mem` 进程写入。

## 5. 使用前提清单

按最小可用到完整功能的顺序勾选：

```text
[ ] Chat 模型端点（OpenAI-compatible /chat/completions，Bearer 鉴权）
[ ] 该端点的 API Key 环境变量（即使 Embedding 用 hash）
[ ] 一个 Bearer Token 及其环境变量（RAM_A_XIAOO_TOKEN）
[ ] 可写的持久化 SQLite 目录（个人记忆库）

语义检索进一步需要：
[ ] Embedding 端点（OpenAI-compatible /v1/embeddings）
[ ] embedding_model + embedding_dimensions（与模型实际输出一致，此后不可中途更换）

可选功能：
[ ] Rerank 端点（OpenRouter 协议，仅 hybrid 模式）
[ ] 图 LLM 端点 + 独立 API Key（Graph Memory）
[ ] 案例库检索 Embedding（案例库功能）
```
