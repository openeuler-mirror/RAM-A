# xiaoO 案例库接入说明

## 接入边界

正式部署中，xiaoO 只连接一个 RAM-A MCP 服务：`ram-a-mem`。
长期记忆和案例库都是同一个 `/mcp` 端口上的 MCP tools：

```text
xiaoO
  -> MCP tools/list / tools/call
  -> ram-a-mem: memory_case_search(query, library?, top_k?)
  -> tenant + library allowlist 映射为私有 dataset_id
  -> ram-a-mem 内置案例库检索
  -> 独立的案例业务库、检索索引和原文目录
```

`dataset_id` 由 RAM-A 服务端配置管理，不会出现在 xiaoO 的工具入参或返回值里。
调用方 token 必须有 `cases:read` 权限，配置中的 library 还必须允许该 token
对应的 `tenant_id`。

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

RAM-A 会移除内部 `dataset_id` 和原始 `source_path`，并将单个引用内容截断到
4000 字符。案例库不可用时返回 `CASE_UNAVAILABLE`（可重试）；未配置、越权或
检索结果无效会返回对应的结构化工具错误。

## RAM-A 服务配置

给 xiaoO 的 RAM-A token 增加权限：

```json
"permissions": ["memory:read", "memory:write", "cases:read"]
```

在 `ram-a-mem` JSON 中打开 RAM-A 长期记忆和案例库 MCP 工具：

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

在同一个 `ram-a-mem` JSON 中配置案例库存储、导入目录、embedding 和 tenant 映射：

```json
"case_library": {
  "rag_store": "data/memory-cases.sqlite",
  "index_store": "data/memory-cases-index.sqlite",
  "source_dir": "crates/memory-cases/test/accuracy_docs",
  "embedding_provider": "hash",
  "embedding_model": "hash",
  "embedding_dimensions": 1024,
  "chunk_size": 160,
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

`ram-a-mem` 启动时会读取 `source_dir`，把新出现的 `.md`/`.txt` 文档导入
`default_library` 对应的 dataset。已经导入过的同名文件会跳过。若不配置
`source_dir`，服务只读取已有持久化案例库 SQLite。

案例库仍然使用独立 SQLite 文件：

- `case_library.rag_store`：保存 dataset、document、task、chunk 和原文路径；
- `case_library.index_store`：保存检索索引；
- `storage.database_path`：保存个人长期记忆和幂等状态。

不要把 `case_library.index_store` 指向 `storage.database_path`。

## 启动

默认配置路径为 `config/ram-a-mem.json`：

```bash
export RAM_A_XIAOO_TOKEN='replace-with-token'
export LLM_API_KEY='replace-with-model-key'
cargo run -p memory-mcp --bin ram-a-mem
```

也可以显式指定：

```bash
RAM_A_MEM_CONFIG=/etc/ram-a/ram-a-mem.json ram-a-mem
```

## xiaoO 端到端验收

验收覆盖真实的 `xiaoO -> RAM-A MCP -> 内置案例库` 路径。密钥只应通过环境变量
注入；不要把 MCP token 或模型 key 写进测试配置和日志。

推荐先把可见工具收窄为案例检索工具，令模型调用路径可重复：

```bash
export RAM_A_XIAOO_TOKEN='xiaoO 访问 RAM-A 的 token'
export LLM_API_KEY='模型服务 key'

cargo run -p xiaoo-endside --bin xiaoo -- \
  --cli \
  --mcp-config /path/to/xiaoo.mcp.json \
  run --config /path/to/xiaoo-cli.toml --debug \
  --tools mcp__ram-a__memory_case_search \
  -p '请先调用案例库工具，查询：WiFi 信号满格但网页打不开，怀疑 DNS 解析异常。必须依据工具返回内容说明原因和处理步骤，并列出案例来源；不要仅凭常识回答。'
```

验收通过判据：

1. xiaoO 日志显示 MCP server connected，且工具数量包含 `memory_case_search`；
2. 有一条成功的 `mcp__ram-a__memory_case_search` tool event；
3. tool result 含预期 `library`、`source_name` 和检索内容，且不含 `dataset_id`、
   `source_path` 或服务端内部路径；
4. 最终回答引用 tool result 中的来源和处理步骤，而不是仅凭常识回答。
