# quick_start_verify.sh 使用说明

`crates/memory-cases/quick_start_verify.sh` 用于快速启动 `memory-cases`，导入一批案例文档，并进入交互问答，用来确认案例库的上传、解析、检索和引用返回是否可用。

## 快速使用

在仓库根目录执行：

```bash
crates/memory-cases/quick_start_verify.sh 案例库目录
```

如果不传文档目录，默认使用：

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

## 常用环境变量

| 环境变量 | 说明 |
| --- | --- |
| `MEMORY_CASES_DOC_DIR` | 未传位置参数时使用的文档目录 |
| `MEMORY_CASES_PORT` | API 监听端口，默认随机 |
| `MEMORY_CASES_CHUNK_SIZE` | 入库切块大小，默认 `160` |
| `MEMORY_CASES_CHAT_TOP_K` | 每次问答取回的引用数，默认 `5` |
| `MEMORY_CASES_API_TOKEN` | REST API 内部 Bearer token；未设置时脚本只为本次运行自动生成 |
| `MEMORY_CASES_KEEP_TMP` | 设置为 `1` 时保留临时目录和日志 |

脚本发出的 `/api/v1/*` 请求都会携带该 token。脚本不会打印 token。
`/health` 可不带 token，其余 REST API 会拒绝未鉴权请求。

## 查看结果

脚本会先打印本次运行配置，例如：

```text
api=http://127.0.0.1:xxxxx
doc_dir=...
doc_count=...
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

重点查看里面的两个日志：

```text
api.log
ingestor.log
```
