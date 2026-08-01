# xiaoO 案例库接入说明

## 接入边界

案例库作为独立的 `memory-cases` REST 服务运行，xiaoO 只连接 RAM-A 的
Streamable HTTP MCP，不直接访问案例 REST API：

```text
xiaoO
  -> MCP tools/list / tools/call
  -> memory-mcp: memory_case_search(query, library?, top_k?)
  -> tenant + library allowlist 映射为私有 dataset_id
  -> memory-cases: POST /api/v1/datasets/{dataset_id}/search
  -> 独立的案例业务库、检索索引和原文目录
```

`dataset_id` 和内部 REST Bearer token 都由 RAM-A 服务端配置管理，不会出现在
xiaoO 的工具入参或返回值里。调用方 token 必须有 `cases:read` 权限，配置中的
library 还必须允许该 token 对应的 `tenant_id`。

## xiaoO 什么时候调用

RAM-A 服务端不会主动判断用户意图或自动触发工具。xiaoO 建立 MCP session 后，
通过 `tools/list` 发现 `memory_case_search`。工具描述会告诉模型它适合查找运维
故障案例、处理证据和来源引用，并强调排障、案例查询、根因分析、处理步骤或相似
历史案例问题应优先调用该工具。

有两种接入方式：

1. 模型自主选工具：如果 xiaoO 已支持标准 MCP tool calling，不需要修改 xiaoO
   代码。模型在用户提出故障现象、排障方法或相似历史案例问题时自行选择该工具。
2. 确定性自动召回：如果要求每次命中某类意图都在回答前查询案例库，需要修改
   xiaoO 的 turn orchestration。建议在 pre-turn 阶段做受控分类，命中排障意图后
   调用 `memory_case_search`，再把有界 references 作为不可信检索上下文注入。

现有 `[memory_automation]` 只负责个人长期记忆的 `memory_search` 和
`memory_ingest`，不等同于案例库自动召回。因此，本次 RAM-A 适配已经提供
模型自主调用能力；确定性自动召回仍是 xiaoO 侧的独立改动。

如果实测发现模型在普通提问中没有稳定调用案例库工具，需要在 xiaoO 的系统提示、
角色提示或演示提示中加入明确引导。可直接复用：

```text
当用户询问故障排查、问题定位、根因分析、处理步骤、运维案例查询或是否存在相似
历史案例时，先调用 RAM-A MCP 工具 `memory_case_search`。

回答时以工具返回的案例引用为主要依据，并列出工具返回的来源名称。如果工具没有
返回相关案例或调用失败，需要先说明这一点，再补充通用排障建议。
```

仓库中也提供了可复制片段：
[`plugins/mcp/case-tool-instruction.md`](../../plugins/mcp/case-tool-instruction.md)。

## MCP 工具契约

调用示例：

```json
{
  "query": "WiFi 信号满格但网页打不开，怀疑 DNS 解析异常",
  "library": "ops",
  "top_k": 5
}
```

- `query` 必填，最多 32000 个 Unicode 字符。
- `library` 可选；省略时使用服务端 `default_library`。它是公开别名，不是
  `dataset_id`。
- `top_k` 可选，默认 `5`，范围 `1..20`。

返回示例：

```json
{
  "library": "ops",
  "references": [
    {
      "chunk_id": "document-1_chunk_0",
      "document_id": "document-1",
      "source_name": "WiFi慢因为DNS解析异常.md",
      "content": "检查 DNS 代理服务并刷新本地 DNS 缓存……",
      "score": 0.82
    }
  ],
  "truncated": false
}
```

RAM-A 会移除案例服务返回的 `source_path` 和 `dataset_id`，限制响应总字节数，
并将单个引用内容截断到 4000 字符。案例服务不可用时返回
`CASE_UNAVAILABLE`（可重试）；未配置、越权或上游响应无效会返回对应的结构化
工具错误。

## 服务配置

给 xiaoO 的 RAM-A token 增加权限：

```json
"permissions": ["memory:read", "memory:write", "cases:read"]
```

在 `ram-a-mcp-server` JSON 中打开 RAM-A 长期记忆和案例库 MCP 工具：

```json
"features": {
  "memory": {
    "enabled": true
  },
  "case_library": {
    "enabled": true
  }
}
```

在 `ram-a-mcp-server` JSON 中配置案例服务和 tenant 映射：

```json
"case_service": {
  "base_url": "http://127.0.0.1:18082",
  "bearer_token_env": "MEMORY_CASES_API_TOKEN",
  "timeout_seconds": 5,
  "max_response_bytes": 262144,
  "default_library": "ops",
  "libraries": [
    {
      "name": "ops",
      "dataset_id": "openeuler-ops-cases",
      "tenant_ids": ["tenant-local"]
    }
  ]
}
```

`memory-cases` API 与 `memory-mcp` 进程必须读取相同的
`MEMORY_CASES_API_TOKEN` 值。该 token 只用于服务间鉴权，不要复用 xiaoO
访问 MCP 的 token。

## 案例现在放在哪里

仓库自带的验证案例原文位于：

```text
crates/memory-cases/test/accuracy_docs
```

`quick_start_verify.sh` 默认导入这批文件，但默认使用临时 SQLite 并在退出时
删除，所以它是验证工具，不是生产部署。运行时持久化位置由两个参数决定：

- `--rag-store`：业务 SQLite，保存 dataset、document、task 和 chunk 元数据；
- `--memory-store`：派生检索索引 SQLite，保存检索文本、FTS 和向量。

上传后的原文保存在 `--rag-store` 所在目录下：

```text
memory-cases-files/{dataset_id}/{document_id}/{file_name}
```

生产接入需要先用持久化 store 创建与 MCP 配置匹配的 dataset（例如
`openeuler-ops-cases`），上传案例并让 ingestor 完成任务。可先运行：

```bash
crates/memory-cases/quick_start_verify.sh
```

验证上传、解析、切块、检索和引用返回的基础链路，再切换到持久化部署。

## xiaoO 端到端验收

下面的验收覆盖真实的 `xiaoO -> RAM-A MCP -> memory-cases` 路径，而不是只用
`curl` 直接调用案例 REST API。密钥只应通过环境变量注入；不要把 MCP token、
案例服务 token 或模型 key 写进测试配置和日志。

### 前置条件

1. 用持久化的 `--rag-store` 和 `--memory-store` 导入案例并启动 ingestor。可先用
   `crates/memory-cases/quick_start_verify.sh` 验证导入、切块和检索；它的默认 store
   会被清理，因此端到端测试应改用单独的持久化 store。
2. 启动 `memory-cases --api`，并将它的 `MEMORY_CASES_API_TOKEN` 与 RAM-A
   `case_service.bearer_token_env` 设为同一个值。
3. 启动 `ram-a-mcp-server`，给 xiaoO 使用的 bearer token 配置 `cases:read`，并使
   token 的 tenant 落入所选 `library` 的 `tenant_ids`。
4. xiaoO 的 MCP JSON 指向 RAM-A 的 `/mcp`，使用 `streamable_http` transport、
   bearer token 与稳定的 `agent_id`。模型配置使用独立的环境变量读取 key。

### CLI：已执行的真实验收

2026-07-30 已使用持久化案例库完成一次真实 CLI 调用：导入了 31 份验证案例，
RAM-A `/healthy`、`/ready` 均为 `ready`；xiaoO 的 MCP 初始化发现了 3 个工具，
并实际执行了 `mcp__ram-a__memory_case_search`。案例命中
`WiFi慢因为DNS解析异常.md` 的两个 chunk，最终答案据此给出 DNS 缓存异常、
`ipconfig /flushdns`、刷新移动端网络以及重启路由器 DNS 代理等步骤，并列出了
案例来源。

推荐先把可见工具收窄为案例检索工具，令模型调用路径可重复：

```bash
export ANTHROPIC_AUTH_TOKEN='由安全的环境或 secret provider 注入'
export RAM_A_E2E_MCP_TOKEN='xiaoO 访问 RAM-A 的 token'

cargo run -p xiaoo-endside --bin xiaoo -- \
  --cli \
  --mcp-config /path/to/xiaoo.mcp.json \
  run --config /path/to/xiaoo-cli.toml --debug \
  --tools mcp__ram-a__memory_case_search \
  -p '请先调用案例库工具，查询：WiFi 信号满格但网页打不开，怀疑 DNS 解析异常。必须依据工具返回内容说明原因和处理步骤，并列出案例来源；不要仅凭常识回答。'
```

如果复用 Anthropic-compatible 的 GLM 代理，xiaoO 的 `api_base` 应包含协议版本段
（例如 `https://model-gateway.example/v1`）；xiaoO 不会像 Claude Code 那样自动补上
`/v1`。验收通过的判据是：

1. CLI 日志出现 `mcp server connected ... count=3`；
2. 有一条成功的 `mcp__ram-a__memory_case_search` tool event；
3. tool result 含预期 `library`、`source_name` 和检索内容，且不含 `dataset_id`、
   `source_path` 或服务间 token；
4. 最终回答引用 tool result 中的来源和处理步骤，而不是仅凭常识回答。

本次 GLM 响应还包含两条空 `tool_name` 调用，xiaoO 将其标记为无效并丢弃；随后
有效的案例检索调用及第二轮回答均正常完成。后续应保留该现象作为模型适配的
兼容性观察，而不是将空调用计为 RAM-A 工具失败。

### TUI：正向用例已执行；其余验收计划待执行

2026-07-31 已完成一次真实 TUI 正向验收：TUI 连接相同的 RAM-A MCP 后，提交本节的
DNS 提示词，界面先显示运行中的 `mcp__ram-a__memory_case_search` 工具卡，随后状态
变为 `done`。最终 transcript 显示了 `WiFi慢因为DNS解析异常.md`、两个引用 chunk
及基于案例的结论和处置步骤。GLM 产生的空工具名调用会显示为失败工具卡；有效案例
工具卡仍会完成，因此该现象目前作为模型输出兼容性观察记录，不算案例库调用失败。

TUI 与 CLI 共用同一份 TOML、MCP JSON、RAM-A token、tenant 和持久化案例库。
在三项服务都已就绪后，以 TUI 默认入口启动：

```bash
export ANTHROPIC_AUTH_TOKEN='由安全的环境或 secret provider 注入'
export RAM_A_E2E_MCP_TOKEN='xiaoO 访问 RAM-A 的 token'

cargo run -p xiaoo-endside --bin xiaoo -- \
  --config /path/to/xiaoo-cli.toml \
  --mcp-config /path/to/xiaoo.mcp.json
```

按以下顺序验证，并保存不含密钥的终端日志或截图：

1. **连接与发现**：启动时确认 RAM-A MCP 初始化成功；若启动时服务不可用，先恢复
   服务再新建 xiaoO runtime/session，避免用已缓存的空工具集继续测。
2. **正向案例检索**：在输入框提交与 CLI 相同的 DNS 提示词。确认 TUI transcript
   出现 `mcp__ram-a__memory_case_search` 工具卡，工具卡显示入参和结果；最终回答
   应包含 `WiFi慢因为DNS解析异常.md`、DNS 根因和案例中的处置步骤。
3. **模型自主选择**：在不把提示词写成“必须调用工具”的情况下，询问相似历史案例
   或具体故障排查方式。确认模型会选择案例检索，而不是误调用个人记忆的
   `memory_search`；如配置 `visible_tools` allowlist，名称必须使用命名空间后的
   `mcp__ram-a__memory_case_search`。
4. **会话生命周期**：关闭并重新打开 TUI，重复同一查询，确认新的 MCP session 可
   再次完成发现和调用，不依赖上一会话的 session id。
5. **受控失败**：分别用缺少 `cases:read` 的 token、tenant 不在 library allowlist
   的 token、以及暂停案例服务三种配置发起请求。TUI 应显示结构化工具失败
   （例如权限拒绝、`CASE_FORBIDDEN`、`CASE_UNAVAILABLE`），不得泄露 dataset、
   文件路径、token 或堆栈；恢复服务并新建 runtime 后，正向用例应再次通过。

TUI 验收通过的最低证据是一次正向工具卡和对应的最终带来源回答；第 3--5 项用于
覆盖模型选择、session 恢复和权限/可用性边界，建议在发布前一并记录结果。
