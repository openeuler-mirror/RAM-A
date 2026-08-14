# quick_start_verify.sh 使用说明

案例仓库：[openeuler/witty-ops-cases](https://atomgit.com/openeuler/witty-ops-cases)

`crates/memory-cases/quick_start_verify.sh` 用于快速启动统一的 `ram-a-mem`，通过其内嵌案例 API 导入一批案例文档，并进入交互问答，用来确认上传、后台 ingestion、检索和引用返回是否可用。`memory-cases` 本身不再是独立进程。

## 快速使用

从 RAM-A 仓库根目录进入脚本所在目录，并克隆 `witty-ops-cases` 案例仓库：

```bash
cd crates/memory-cases
git clone https://atomgit.com/openeuler/witty-ops-cases.git
```

在当前目录执行以下命令，导入 `community_maintenance` 案例库并启动交互验证：

```bash
sh quick_start_verify.sh witty-ops-cases/community_maintenance/
```

也可以传入其他案例库目录：

```bash
sh quick_start_verify.sh 案例库目录
```

如果不传案例库目录，默认使用：

```text
crates/memory-cases/test/accuracy_docs
```

脚本启动完成后会进入交互模式：

```text
问题>
```

直接输入问题即可验证检索效果，例如：

```text
打印机显示缺纸但纸盒有纸，怎么办？
```

退出输入：

```text
:q
```

也支持 `:quit`、`exit`、`quit`。

## 支持格式

当前支持两类文档：

```text
Markdown: .md / .markdown / .mdx
Text:     .txt / .text / .log
```

`.log` 会按纯文本导入。脚本只扫描目录第一层，并导入该层下的全部支持文档。

## 常用参数

固定端口，方便另开终端访问 API：

```bash
MEMORY_CASES_PORT=18080 \
  crates/memory-cases/quick_start_verify.sh /root/atomgit/witty-ops-cases/community_maintenance
```

保留临时目录和日志，方便排查问题：

```bash
MEMORY_CASES_KEEP_TMP=1 \
  crates/memory-cases/quick_start_verify.sh /root/atomgit/witty-ops-cases/community_maintenance
```

默认使用本地 hash embedding，适合离线 smoke 和演示。如果要验证真实或本地部署的
OpenAI-compatible embedding 服务：

```bash
export LOCAL_EMBEDDING_API_KEY='replace-with-provider-key-or-dummy-if-local-service-ignores-auth'
MEMORY_CASES_EMBEDDING_PROVIDER=openai_compatible \
MEMORY_CASES_EMBEDDING_API_KEY_ENV=LOCAL_EMBEDDING_API_KEY \
MEMORY_CASES_EMBEDDING_BASE_URL=http://127.0.0.1:8000/v1 \
MEMORY_CASES_EMBEDDING_MODEL=local-embedding-model \
MEMORY_CASES_EMBEDDING_DIMENSIONS=1024 \
  crates/memory-cases/quick_start_verify.sh /root/atomgit/witty-ops-cases/community_maintenance
```

## 常用环境变量

| 环境变量 | 说明 |
| --- | --- |
| `MEMORY_CASES_DOC_DIR` | 未传位置参数时使用的文档目录 |
| `MEMORY_CASES_PORT` | API 监听端口，默认随机 |
| `MEMORY_CASES_RAG_STORE` | 业务 SQLite 路径，保存 dataset、document、task 和 chunk |
| `MEMORY_CASES_MEMORY_STORE` | 检索索引 SQLite 路径，保存 memory text、FTS 和 embedding |
| `MEMORY_CASES_CHUNK_SIZE` | 入库切块大小，默认 `160` |
| `MEMORY_CASES_CHAT_TOP_K` | 每次问答取回的引用数，默认 `5` |
| `MEMORY_CASES_EMBEDDING_PROVIDER` | `hash` 或 `openai_compatible`，默认 `hash` |
| `MEMORY_CASES_EMBEDDING_DIMENSIONS` | embedding 维度，默认 `256`；必须与当前案例索引库里已有记录维度一致 |
| `MEMORY_CASES_EMBEDDING_API_KEY_ENV` | `openai_compatible` 使用的 API key 环境变量名，默认 `OPENAI_API_KEY` |
| `MEMORY_CASES_EMBEDDING_BASE_URL` | `openai_compatible` 的 OpenAI-compatible `/v1` base URL |
| `MEMORY_CASES_EMBEDDING_MODEL` | `openai_compatible` 的 embedding 模型名 |
| `MEMORY_CASES_API_TOKEN` | REST API 内部 Bearer token；未设置时脚本只为本次运行自动生成 |
| `MEMORY_CASES_KEEP_TMP` | 设置为 `1` 时保留临时目录和日志 |

推荐把 `MEMORY_CASES_MEMORY_STORE` 指向案例库自己的索引库，例如
`data/memory-cases-index.sqlite`，不要和 RAM-A 长期记忆的
`data/ram-a-memory.sqlite` 共用一个 SQLite 文件。分库可以隔离个人记忆和案例索引
写入，也能让案例库重建/清理索引时不影响用户长期记忆。

如果为了小规模 smoke test 故意共用同一个 SQLite 文件，案例库和 RAM-A MCP 必须
使用同一套 embedding provider/base URL/model/API key 环境变量名和维度。
`memory-core` 会在新写入记录里保存 embedding profile，并拒绝同一 `scope_id`
下混用不同 profile；即使两个模型维度相同，也不能认为它们处于同一语义向量空间。

脚本发出的 `/api/v1/*` 请求都会携带该 token。脚本不会打印 token。
统一健康端点 `/healthy` 可不带 token，其余案例 REST API 会拒绝未鉴权请求。

## 查看结果

脚本会先打印本次运行配置，例如：

```text
api=http://127.0.0.1:xxxxx
doc_dir=...
doc_count=...
embedding_provider=...
embedding_dimensions=...
chunk_size=...
chat_top_k=...
```

进入 `问题>` 后，每次提问会输出答案和引用片段。重点看：

- 答案是否覆盖你关心的处理结论。
- 引用片段是否来自预期案例文档。
- 引用内容是否包含关键证据或操作步骤。

## 排查问题

如果脚本失败，先用 `MEMORY_CASES_KEEP_TMP=1` 重跑：

```bash
MEMORY_CASES_KEEP_TMP=1 \
  crates/memory-cases/quick_start_verify.sh /root/atomgit/witty-ops-cases/community_maintenance
```

退出时会打印保留的临时目录：

```text
kept tmp: /tmp/memory-cases-quick-verify.xxxxxx
```

重点查看统一服务日志：

```text
api.log
```
