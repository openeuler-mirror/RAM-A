# memory-cases QA Eval 测试说明

本文说明 `crates/memory-cases/qa_eval_test.sh` 的执行原理、`crates/memory-cases/test/qa_cases.jsonl`
的用例格式、当前测试集覆盖范围、优缺点，以及它和外部检索测试的区别。

这套测试目前更适合作为“诊断尺子”，不是只追求全绿的 smoke test。也就是说，新增的部分用例允许暴露当前实现还不满足的能力边界。

## 1. 测试目标

`qa_eval_test.sh` 验证的是一条端到端 RAG 链路：

```text
启动 API
-> 创建 dataset
-> 上传测试文档
-> 启动 ingestor
-> 等待解析入库
-> 检查 chunks
-> 对每条 QA case 调 /search 和 /chat/completions
-> 校验召回来源、引用内容、答案摘要和 no-hit 行为
-> 输出 JSON 报告
```

它关注的问题不是“LLM 最终回答有多像标准答案”，而是：

- 文档能不能被正确上传和解析；
- Markdown / text / log 是否都能进入索引；
- 检索是否能召回期望文档；
- chat references 是否保留期望文档；
- 现象和解决方案分离时，命中文档后是否能带回解决方案 chunk；
- 多文档同症状问题是否能召回多个原因；
- 无关问题是否会误召回。

## 2. 执行原理

脚本入口是：

```bash
bash crates/memory-cases/qa_eval_test.sh
```

主要流程如下。

### 2.1 准备隔离环境

每次运行都会创建临时 SQLite，并默认拆成业务库和检索索引库：

```text
RAG_STORE=${TMP_DIR}/memory-cases.sqlite
MEMORY_STORE=${TMP_DIR}/memory-cases-index.sqlite
```

这样可以避免历史入库数据影响召回结果。API 和 ingestor 使用同一组 `--rag-store`
和 `--memory-store`。其中 `rag-store` 保存 dataset、document、task、chunk 等业务事实；
`memory-store` 保存 `memories`、`memory_fts` 和 embedding，是由入库、更新、删除流程维护的
派生检索索引。文档检索记录使用 `memory_index_namespace = "memory-cases"`，用于避免文档清理
误删同库里的用户长期记忆。

默认输入：

| 名称 | 默认值 | 说明 |
| --- | --- | --- |
| `MEMORY_CASES_DOC_DIR` | `crates/memory-cases/test/accuracy_docs` | 测试文档目录 |
| `MEMORY_CASES_QA_CASES` | `crates/memory-cases/test/qa_cases.jsonl` | QA case 文件 |
| `MEMORY_CASES_QA_REPORT` | `outputs/memory-cases/qa_eval_report.json` | 评测报告 |
| `MEMORY_CASES_QA_RAG_STORE` | `${TMP_DIR}/memory-cases.sqlite` | 业务 SQLite，保存 RAG 元数据和 chunks |
| `MEMORY_CASES_QA_MEMORY_STORE` | `${TMP_DIR}/memory-cases-index.sqlite` | 检索索引 SQLite，保存 memory text、FTS 和 embedding |
| `MEMORY_CASES_API_TOKEN` | 本次运行自动生成 | REST API 内部 Bearer token；外部设置时使用给定值 |
| `MEMORY_CASES_QA_CHUNK_SIZE` | `160` | 测试用 chunk size |
| `MEMORY_CASES_QA_MAX_DOCS` | `0` | 本地调试时限制上传文档数量，0 表示不限制 |
| `MEMORY_CASES_QA_MIN_SOLUTION_TERM_CASES` | `1` | 声明 `required_solution_terms` 的 case 数量下限 |

`CHUNK_SIZE=160` 是有意设置得偏小，目的是让问题现象、原因、解决方案更可能分散到不同 chunk，从而测试文档级召回和同文档 chunk 展开是否有效。

QA 脚本默认不启用 LLM 摘要。chunk token 计数使用 tiktoken `cl100k_base`，
不依赖网络下载 tokenizer 文件。需要测试 LLM 摘要链路时，可以额外设置：

```bash
MEMORY_CASES_SUMMARY_LLM_MODEL="gpt-4o-mini"
OPENAI_API_KEY="sk-..."
MEMORY_CASES_SUMMARY_LLM_BASE_URL="https://api.openai.com/v1"
```

### 2.2 前置校验

脚本会检查：

- `cargo`、`curl`、`find`、`sort`、`python3` 是否存在；
- 文档目录、case 文件、runner 文件是否存在；
- case 数量是否大于 0；
- 声明 `required_solution_terms` 的 case 数量是否达到下限；
- 测试文档是否同时包含 Markdown 和纯文本类格式。

文档收集支持：

```text
Markdown: .md / .markdown / .mdx
Text:     .txt / .text / .log
```

即使设置了 `MEMORY_CASES_QA_MAX_DOCS`，脚本也会把 case 中声明的 `expected_sources` 强制追加进上传列表，避免因为调试限量导致假失败。

### 2.3 启动服务和入库

脚本先启动 API：

```bash
export MEMORY_CASES_API_TOKEN='replace-with-an-internal-test-token'
cargo run -p memory-cases -- --api \
  --bind 127.0.0.1:${PORT} \
  --rag-store "$RAG_STORE" \
  --memory-store "$MEMORY_STORE" \
  --chunk-size "$CHUNK_SIZE"
```

实际执行脚本时，如果外部没有设置 `MEMORY_CASES_API_TOKEN`，脚本会为本次
评测自动生成一个且不会打印。所有 `/api/v1/*` 请求（包括 Python QA
runner）都会携带该 Bearer token。

然后创建固定 dataset：

```text
dataset_id = qa-eval-dataset
```

接着逐个上传文档。每个文档都会带固定的 `document_id` 和 `task_id`，方便后续轮询任务状态。

上传完成后再启动 ingestor：

```bash
cargo run -p memory-cases -- --ingestor \
  --rag-store "$RAG_STORE" \
  --memory-store "$MEMORY_STORE" \
  --chunk-size "$CHUNK_SIZE" \
  --poll-ms 100
```

脚本会轮询所有 task，直到它们变成 `completed`。如果任一 task 失败，会打印 API 和 ingestor 的最近日志。

### 2.4 检查 chunks

所有文档入库完成后，脚本会调用：

```text
GET /api/v1/datasets/{dataset_id}/documents/{document_id}/chunks
```

要求每篇上传文档至少生成一个 chunk。这一步只证明“上传 -> parser -> chunker -> repo”链路没有断。

注意，这里不是检索测试。`/documents/{document_id}/chunks` 是按已知 `document_id` 直接读取
`rag_chunks`，不会使用用户 query，也不会经过 `memories`、`memory_fts`、BM25/dense/hybrid
召回、相关性过滤、top_k 排序或文档级展开。它只能证明文档已经被解析并落库，不能证明用户问题能把这篇文档搜出来。

### 2.5 执行 QA cases

真正的 case 评测由 `test/qa_eval_runner.py` 完成。每个 case 会请求两条接口：

```text
POST /api/v1/datasets/{dataset_id}/search
POST /api/v1/chat/completions
```

`/search` 用来检查底层检索结果，`/chat/completions` 用来检查对外问答接口返回的 `references` 是否仍然保留目标来源。

因此 2.4 和 2.5 的检查目的不同：

- 2.4 检查“目标文档有没有 chunks 可用”；
- 2.5 的 `/search` 检查“给定 question 后，检索系统能不能从全库找回期望来源”；
- 2.5 的 `/chat/completions` 检查“对外 QA 接口有没有把 search 结果正确带进 references”。

报告写入：

```text
outputs/memory-cases/qa_eval_report.json
```

报告中会保存每条 case 的检查项、缺失来源、意外来源、缺失关键词、search 命中片段和 chat references 片段预览。

## 3. qa_cases.jsonl 格式

文件名保留为 `.jsonl`，但当前 runner 只支持一种格式：缩进 JSON 数组。这样便于人工阅读、review 和批量维护。

### 3.1 字段说明

| 字段 | 类型 | 必填 | 说明 |
| --- | --- | --- | --- |
| `id` | string | 否 | 用例 ID；建议使用中文覆盖点，例如 `精度_硬负例_名称解析`。不要使用全局连续序号，避免中间插入用例时重命名已有 ID |
| `question` | string/string[] | 是 | 用户查询；过长时可写成字符串数组，runner 会用换行拼回字符串 |
| `standard_answer` | string/string[] | 否 | 标准答案说明；过长时可写成字符串数组；当前不做语义打分，主要用于报告阅读 |
| `expected_sources` | string[] | 条件必填 | 期望召回的一篇或多篇文档文件名 |
| `expect_no_hits` | bool | 否 | 是否期望无检索结果 |
| `required_answer_terms` | string/string[] | 否 | chat answer 中必须出现的词 |
| `required_reference_terms` | string/string[] | 否 | 所有 references 合并后必须出现的词 |
| `required_solution_terms` | string/string[] | 否 | 期望来源 references 中必须出现的解决方案词 |
| `top_k` | number | 否 | search/chat 请求使用的最大返回数，默认 5；测试不要求必须返回满 top_k |

### 3.2 普通命中 case

```json
{
  "id": "精度_硬负例_名称解析",
  "question": "Wi-Fi 满格但网页提示 ERR_NAME_NOT_RESOLVED，DNS 解析失败怎么处理？",
  "expected_sources": [
    "WiFi慢因为DNS解析异常.md"
  ],
  "required_answer_terms": [
    "Wi-Fi",
    "DNS"
  ],
  "required_reference_terms": [
    "ERR_NAME_NOT_RESOLVED",
    "DNS 代理缓存"
  ],
  "required_solution_terms": [
    "ipconfig /flushdns",
    "飞行模式",
    "DNS 代理服务"
  ],
  "top_k": 2
}
```

这个 case 会检查：

- `/search` 是否召回 `D11`；
- `/chat/completions.references` 是否保留 `D11`；
- answer 是否包含 `Wi-Fi`、`DNS`；
- references 是否包含证据词；
- `D11` 的 references 是否包含解决方案词；
- references 是否没有混入 `expected_sources` 之外的额外来源。

### 3.3 解决方案覆盖 case

`required_solution_terms` 表示：这个 case 不只要求检索命中文档，还要求期望来源的 references 中包含解决方案词。

它用于覆盖这类场景：用户问题主要描述现象，但解决方案词和问题词重叠较少。没有这个字段时，case 只能测“是否命中文档”，测不到“是否带回同文档解决方案 chunk”。

### 3.4 no-hit case

```json
{
  "id": "拒答_无关问题无命中",
  "expect_no_hits": true,
  "question": "多肉植物多久浇一次水比较好？",
  "top_k": 5
}
```

no-hit case 会检查：

- `/search` 没有 chunks；
- `/chat/completions` 没有 references；
- answer 中包含“没有检索到”。

## 4. 当前覆盖场景

### 4.1 基础检索

- 单文档强匹配：打印机缺纸。
- 单文档现象-方案分离：电脑开机慢。
- 多文档现象-方案分离：视频会议卡顿的摄像头、Wi-Fi、后台下载三类原因。

### 4.2 多文档同症状

- 泛 Wi-Fi 慢问题要求召回多个可能原因：
  - 信道拥挤；
  - 后台上传；
  - DNS 解析异常；
  - 运营商外网丢包；
  - 路由器过热降速。

这类 case 用来测试“同一个症状下的多原因召回”，不是只测 top-1 精准命中。

### 4.3 无标题和弱结构文档

- `D09-客服记录-蓝牙外设.txt`：自然客服记录，没有 `标题/现象/原因/解决方案` 模板。
- `D10-2026-07-01值班记录.txt`：泛文件名，问题只在正文里。
- `D15-会议室投屏延迟长记录.txt`：长文档，答案在后段。
- text FAQ：移动端验证码收不到，查询里使用 `OTP`、后台状态和用户描述混写。
- text 值班流水：数据库连接池耗尽，现象、误排方向、根因、处理结论分散在时间线里。
- text 配置说明：反向代理上传 `413`，正文包含 nginx 配置片段和后端参数。
- text 日志摘录：systemd timer 与 crontab 同时触发，要求从日志时间线判断重复执行。
- text 会议纪要：共享盘权限继承异常，正文包含参会人、讨论结论和执行事项。
- text 多轮客服：邮箱附件被 DLP 拦截，需要从多轮记录中识别策略名和放行约束。
- text 告警记录：`df -h` 空间足够但 inode 耗尽，用于区分容量不足和 inode 不足。
- text 终端记录：SSH 主机指纹变更，要求先校验资产身份再更新 `known_hosts`。
- text 社区运维案例：`.txt` 内部使用 `#标题/#问题现象/#问题根因/#解决方案` 这类井号标题结构。
- text 版本化案例：`openEuler 24.03 SP2`、`openEuler 22.03 LTS SP4` 等版本词和故障特征一起参与召回。
- text 重复故障名：两篇 `IRQ 352 亲和性修改失败` 只靠根因证据区分，覆盖同标题硬负例。

### 4.4 硬负例和精度压力

- 多篇都是 Wi-Fi 慢，但根因不同。
- 扫描仪 case 设置 `top_k=3`，用于观察目标命中后是否会因为凑满数量而混入无关来源。
- 投影仪和视频会议类文档之间存在相似词，用于检查错召。
- IRQ 352 两篇 text 文档标题高度相似，但一个是 `managed IRQ` 限制，一个是 `irqbalance` 覆盖人工配置。
- 近域 no-hit：openEuler 发版计划问题不应强行召回故障维护案例。

### 4.5 查询表达多样性

- 错别字：`蓝芽` vs `蓝牙`。
- 中英混合：`VPN`、`2FA`、`MFA token expired`。
- 运维中英混合：`HikariPool`、`DLP-ENCRYPTED-ARCHIVE`、`StrictHostKeyChecking`。
- 日志查询：`gateway`、`payment-api`、`tls handshake timeout`。
- 错误码：`rsync error code 23`、`413 Request Entity Too Large`、`No space left on device`、`Curl error 28`。
- 函数名和路径：`pidns_update_load_tasks`、`/etc/yum.repos.d/openEuler.repo`、`/proc/irq/352/smp_affinity_list`。

### 4.6 Markdown 多结构

- 无 H1，仅有二级标题；
- Markdown 表格；
- 列表；
- fenced code block；
- 破损 Markdown：未闭合 code fence。

### 4.7 no-hit

- 多肉浇水问题用于检查无关查询是否被拒绝，而不是硬召回办公/网络文档。
- openEuler 发版计划问题用于检查“领域相近但资料集中不存在答案”的拒答能力。

## 5. 当前测试结果怎么理解

因为该测试集包含诊断型用例，所以不要求当前实现全部通过。

一次扩充 text 用例后的运行结果示例：

```text
qa eval result: 21/29 passed
solution terms result: 21/27 passed
no-hit result: 0/2 passed
```

实际通过数以新生成的 `qa_eval_report.json` 为准。

这不是测试集失败，而是在暴露当前能力缺口。当前主要缺口包括：

- 英文-only 文档和中英混合 query 的召回不稳；
- `.log` 英文日志召回不稳；
- no-hit 过滤不稳，无关植物问题仍可能召回网络文档。
- text 语料扩大后，部分单文档查询会混入跨域相似来源，例如蓝牙外设问题混入 IRQ 案例。
- 领域相近但知识库没有答案的问题仍会被强行召回，例如 openEuler 发版计划问题召回故障案例。

如果将这套测试接入 CI，需要先决定策略：

- 作为强回归门禁：需要引入 expected-fail 或最低通过率阈值；
- 作为诊断报告：允许脚本返回非零，用报告追踪能力变化；
- 作为分层测试：基础 case 必须全过，诊断 case 单独统计。

## 6. 优点

1. 端到端覆盖真实链路
   它不是只测 parser 或 search 函数，而是完整跑 API、上传、任务入库、chunk 检查、search、chat references。

2. 输入固定，可复现
   文档和 case 都在仓库里，临时 SQLite 每次重建，便于复现同一批问题。

3. 同时检查 recall 和 precision
   `expected_sources` 检查漏召，runner 默认要求 references 不混入额外来源，用来检查误召。

4. 能验证 references
   不是只看 answer 文本，而是确保 chat 对外返回的引用来源没有丢失。

5. 能覆盖“命中文档但答案 chunk 分散”的情况
   小 chunk size 加 `required_solution_terms` 可以测试同文档扩展是否带回解决方案。

6. 不依赖外部 LLM
   当前 answer 是服务拼出的检索摘要，测试成本低、速度快、离线可跑。

## 7. 缺点

1. 不是严格的答案质量评测
   `standard_answer` 目前主要用于人工阅读，runner 没有做语义相似度、faithfulness 或 LLM judge。

2. 关键词断言比较硬
   `required_*_terms` 对措辞、大小写和截断敏感，不能表达“语义等价但字面不同”的情况。

3. 当前 embedding 是本地 HashEmbedding
   它适合离线 smoke/回归，但不能代表真实语义 embedding 的召回能力。

4. 没有 expected-fail / threshold 机制
   诊断型 case 失败时脚本会整体返回非零，不适合作为“必须全绿”的 CI 门禁。

5. 文档规模仍然小
   目前只有 18 篇文档，不能代表大规模知识库的排序压力、性能压力和长尾格式。

6. 格式覆盖有限
   当前 parser 主要支持 Markdown 和纯文本，所以还没有覆盖 PDF、DOCX、PPT、图片 OCR、复杂表格等现实格式。

7. 没有并发和性能指标
   不测吞吐、延迟、批量入库、重复上传、任务恢复和数据库竞争。
