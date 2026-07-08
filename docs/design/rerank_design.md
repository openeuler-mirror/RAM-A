# RAM-A Rerank 设计

> 状态：调研与实现设计草案，2026-07-06。
>
> 范围：为 RAM-A 在现有 dense/BM25/hybrid 检索之后增加可选的 learned
> rerank 阶段。本文只描述设计，不代表当前代码已经实现。

## 1. 结论

RAM-A 当前已经支持 dense 语义检索、BM25 关键词检索和加权 hybrid fusion，
但还没有真正的 rerank 小模型。当前所谓排序只是把 dense 分数和 BM25 分数归一
化后加权求和，再截断到 `top_k`。

第一版 rerank 已确认使用 OpenRouter Rerank API。Rust 进程内不直接加载
HuggingFace / Transformers 模型，而是通过 HTTP client 调托管 rerank 服务：

```text
query
-> dense candidates
-> BM25 candidates
-> deterministic hybrid fusion
-> 取 rerank_input_k 条候选
-> reranker 模型给 (query, memory_text) 打分
-> 返回最终 top_k
```

第一版默认决策：

```text
rerank.enabled = false        # 库和 CLI 默认关闭，保持旧 baseline 可复现
rerank.provider = openrouter  # 第一版只实现 OpenRouter HTTP provider
rerank.model = cohere/rerank-v3.5
rerank.api_key_env = OPENROUTER_API_KEY
rerank.base_url = https://openrouter.ai/api/v1
candidate_k = 100             # dense / BM25 各取 100，沿用当前默认量级
rerank_input_k = 40           # hybrid 融合后送入 reranker 的候选数
answer_top_k = 5              # 给回答模型的上下文数量，评估指标另按 benchmark 要求
```

`rerank_input_k = 40` 作为第一版默认值。它不是行业固定标准，而是结合当前 RAM-A
默认候选池、外部长期记忆/检索系统的候选倍率，以及 cross-encoder 延迟成本得出
的折中起点。后续可以用 `20 / 40 / 80` 做 ablation，但第一版实现按 40 落地。

跑分时应该显式打开 rerank，并要求 `OPENROUTER_API_KEY` 可用。默认命令保持旧
hybrid，rerank 版本的 benchmark 脚本或命令必须带 `--rerank`。benchmark 中
reranker 出错默认 fail closed，也就是让本次 search / benchmark 失败，避免把
退化后的 hybrid 结果误标成 rerank 结果。

## 2. 当前 RAM-A 检索状态

当前核心检索路径是确定性的，不包含 learned reranker：

1. `SearchMode::Hybrid` 是默认检索模式。
2. `RetrievalConfig::candidate_limit(top_k)` 返回显式 `candidate_k`，否则使用
   `max(top_k * 5, 100)`。
3. hybrid search 会对 query 做 embedding，然后分别取 dense candidates 和
   BM25 candidates。
4. 按 memory id 合并候选。
5. 分别归一化 dense score 和 BM25 score。
6. 计算：

```text
final_score = embedding_weight * dense_norm + bm25_weight * bm25_norm
```

7. 按 `final_score` 排序，直接截断到 `top_k`。

关键区别：

```text
当前 weighted rerank = 分数融合 + 排序。
真正 learned rerank = 模型读取 query 和候选文本，再判断相关性。
```

所以当前系统可以把候选结果缩到 `top_k`，但这不是“rerank 小模型从 40 条判断到
5 条”的路线。

相关代码位置：

- `crates/memory-core/src/api.rs`：`RetrievalConfig`、`SearchMode`、
  `candidate_k`。
- `crates/memory-core/src/manager.rs`：`search_hybrid_with_progress`、
  `dense_candidates`、`fuse_hybrid_candidates`。
- `crates/memory-bench/src/main.rs`：CLI 已暴露 `--candidate-k`、
  `--embedding-weight`、`--bm25-weight`，但没有 rerank 参数。

## 3. 什么是真正的 learned rerank

dense retrieval 和 BM25 是第一阶段召回器。它们的职责是快，并且尽量别漏掉可能
相关的记忆。reranker 是第二阶段精排模型。它不只看向量距离或关键词，而是直接
读入 query 和候选文本，判断候选是否真的能回答问题。

典型 cross-encoder 输入：

```text
query: "用户现在更喜欢喝什么咖啡？"
document: "用户以前常点拿铁，但上周说现在更喜欢手冲咖啡。"
```

模型输出一个 relevance score。这个分数通常比 cosine similarity 和 BM25 更擅
长处理：

- 候选文本是否真的回答了问题；
- 新旧偏好是否冲突；
- 否定、时间、转折等语义；
- 关键词命中但语义不相关的干扰项；
- 语义相近但信息不够具体的候选。

代价是：cross-encoder 的成本大致随候选数线性增长。40 条就是 40 个
`(query, document)` pair；80 条大致是两倍工作量；200 条通常会让 rerank 成为
检索链路里的主要延迟来源。

## 4. 外部产品和框架调研

### 4.1 Mem0

Mem0 的搜索文档把 search 描述成语义检索、过滤、阈值和可选 reranking 的组合。
Platform 版本可以通过 `rerank=True` 打开 rerank；OSS 版本需要配置本地或第三方
reranker。

Mem0 文档里的搜索默认值：

```text
top_k = 10
threshold = 0.1
rerank = false
```

Mem0 OSS 的检索也会融合 semantic、BM25 和 entity signal。当前 OSS 实现里，
hybrid 阶段会 over-fetch：

```text
internal_limit = max(limit * 4, 60)
```

也就是说，如果外部请求 `limit = 10`，内部至少会先拿 60 条候选来做融合。

Mem0 的 reranker 实现包括：

- SentenceTransformer cross-encoder，默认模型
  `cross-encoder/ms-marco-MiniLM-L-6-v2`。
- HuggingFace sequence-classification reranker，默认模型
  `BAAI/bge-reranker-base`。
- Cohere rerank API。

它的 reranker 接口接受 `query`、`documents` 和可选 `top_k`，输出带
`rerank_score` 的文档。实现会按模型分数排序并应用 `top_k`。如果 reranker 调用
失败，Mem0 的具体实现会 fallback 到原始顺序。

对 RAM-A 的启发：

- rerank 应该是可选阶段。
- provider 应该可替换：本地模型、HTTP 服务、托管 API 都应能接。
- 旧 hybrid baseline 应保留。
- 失败策略要区分场景：benchmark 不应该悄悄 fallback；交互式产品可以选择
  fail-open。

参考：

- https://github.com/mem0ai/mem0/blob/main/docs/core-concepts/memory-operations/search.mdx
- https://github.com/mem0ai/mem0/blob/main/docs/api-reference/memory/search-memories.mdx
- https://github.com/mem0ai/mem0/blob/main/mem0/reranker/base.py
- https://github.com/mem0ai/mem0/blob/main/mem0/reranker/sentence_transformer_reranker.py
- https://github.com/mem0ai/mem0/blob/main/mem0/reranker/huggingface_reranker.py
- https://github.com/mem0ai/mem0/blob/main/mem0/reranker/cohere_reranker.py

### 4.2 Zep / Graphiti

Graphiti 是图记忆系统。它的 search 支持多种召回方式：

- BM25；
- cosine similarity；
- BFS graph expansion。

它也支持多种 reranker：

- RRF：reciprocal rank fusion；
- MMR：maximal marginal relevance；
- node distance、episode mentions 等图结构 reranker；
- cross-encoder rerank。

Graphiti 默认 search limit 是 `10`。在执行搜索时，每种 search method 通常取
`2 * limit` 条候选。cross-encoder 的 edge/node recipe 常见返回 `limit = 10`；
community cross-encoder recipe 则更小，常见是 `limit = 3`。

对 RAM-A 的启发：

- “rerank” 不一定等于小模型。RRF/MMR 是确定性排序融合；cross-encoder 才是
  learned rerank。
- 多路召回后取一个较小的候选池再返回 10 条左右，是常见设计。
- 图结构 rerank 对 Graphiti 很重要，但 RAM-A 当前是 memory record store，
  第一阶段应先实现文本 cross-encoder rerank。

参考：

- https://github.com/getzep/graphiti/blob/main/graphiti_core/search/search_config.py
- https://github.com/getzep/graphiti/blob/main/graphiti_core/search/search_config_recipes.py
- https://github.com/getzep/graphiti/blob/main/graphiti_core/search/search.py

### 4.3 LlamaIndex

LlamaIndex 把 reranker 设计成 retrieval 之后、response synthesis 之前的 node
postprocessor。官方示例是：

```text
similarity_top_k = 10
reranker top_n = 3
```

也就是先取 10 条，再由 reranker 压到 3 条给 LLM。

LlamaIndex 文档建议无 API key 的本地方案可以用 SentenceTransformer
cross-encoder；托管 API 可以用 Cohere、Jina、Voyage、Mixedbread；如果追求质
量且不在意延迟，可以用 LLM-based reranker。

对 RAM-A 的启发：

- retrieval 和 rerank 应该是两个独立阶段。
- `rerank_input_k` 和最终 `top_k` 应该是两个不同参数。
- 本地 cross-encoder 或 HTTP rerank service 是现实的第一版实现。

参考：

- https://github.com/run-llama/llama_index/blob/main/docs/src/content/docs/framework/module_guides/models/rerankers.md
- https://github.com/run-llama/llama_index/blob/main/docs/src/content/docs/framework/optimizing/basic_strategies/basic_strategies.md
- https://github.com/run-llama/llama_index/blob/main/llama-index-integrations/postprocessor/llama-index-postprocessor-sbert-rerank/llama_index/postprocessor/sbert_rerank/base.py

### 4.4 LangChain

LangChain 的 Cohere reranker 是一个 document compressor：上游 retriever 先返回
候选文档，CohereRerank 再压缩成少量结果。

当前 `langchain_cohere.CohereRerank` 的默认值：

```text
top_n = 3
```

候选数量由前面的 retriever 控制。这和 RAM-A 应采用的设计一致：第一阶段候选数
和最终 rerank 输出数不应该混成一个参数。

参考：

- https://github.com/langchain-ai/langchain-cohere/blob/main/libs/cohere/langchain_cohere/rerank.py
- https://github.com/langchain-ai/langchain/blob/main/libs/langchain/langchain_classic/retrievers/document_compressors/cohere_rerank.py

### 4.5 Letta / MemGPT

Letta 的 archival memory search 文档描述为语义相似度检索。工具层默认：

```text
archival_memory_search top_k = 10
conversation_search default page size = 5
```

这一路径没有看到明确的 cross-encoder rerank 阶段。这个反例也有价值：长期记
忆系统不一定都默认上 learned rerank，因为它会引入延迟、服务依赖和失败策略。

对 RAM-A 的启发：

- rerank 不应该强制默认开启。
- 对低延迟交互场景，semantic/hybrid 检索仍应能单独运行。

参考：

- https://github.com/letta-ai/letta/blob/main/letta/functions/function_sets/base.py
- https://github.com/letta-ai/letta/blob/main/letta/services/tool_executor/core_tool_executor.py
- https://github.com/letta-ai/letta/blob/main/letta/constants.py

## 5. 为什么第一版选 40 条 rerank

`40` 是第一版默认值，基于下面几个约束。

### 5.1 当前 RAM-A 默认候选池大于应 rerank 的规模

当前默认：

```text
candidate_k = max(top_k * 5, 100)
```

如果最终 answer context 只需要 `top_k = 5`，当前 hybrid 仍可能先取：

```text
dense top 100
BM25 top 100
union <= 200
```

如果把 union 后最多 200 条都送给 reranker，CPU、本地模型或托管 API 都可能过慢。
所以 learned rerank 需要一个中间层：先由 hybrid 分数裁到 `rerank_input_k`，再
交给模型判断。

### 5.2 外部系统一般不会把大量候选都交给模型

观察到的模式：

```text
LlamaIndex 示例：10 -> 3
LangChain CohereRerank 默认输出：3
Graphiti：常见每路 2 * limit，最终 limit 约 10
Mem0 OSS hybrid over-fetch：max(limit * 4, 60)
```

如果最终给回答模型 5 条记忆，`40 -> 5` 是 8 倍候选比。它比 `10 -> 3` 更保守，
但比 rerank 当前 RAM-A 默认 union 的 100-200 条要轻很多。

### 5.3 cross-encoder 延迟随候选数线性增长

cross-encoder 要对每个候选构造一个 `(query, candidate_text)` pair。即使 batch
inference 能提高吞吐，候选数仍是主要成本因子。

粗略关系：

```text
20 条：延迟最低，但可能漏掉 hybrid 排名靠后的正确证据
40 条：候选空间足够大，通常仍能用少数 batch 处理
80 条：召回更稳，但延迟大致接近 40 条的两倍
200 条：可能让 rerank 成为搜索链路的主要瓶颈
```

因此第一版固定 `rerank_input_k = 40`，并在 metrics 里记录 rerank 用时。如果
40 的质量收益不够，再做 80；如果 40 太慢，再评估 20。

### 5.4 benchmark 风险

候选太少，reranker 没机会看到正确记忆；候选太多，延迟上升，还会带来更多干扰
项。第一版需要把 `40` 当默认，而不是当结论。

建议后续对照实验：

```text
rerank_input_k = 20
rerank_input_k = 40
rerank_input_k = 80
```

但实现先按 40 做，避免一次性把配置空间扩大到不可控。

## 6. 推荐配置

### 6.1 库和 CLI 默认

默认保持当前行为：

```text
rerank.enabled = false
rerank.provider = openrouter
rerank.model = cohere/rerank-v3.5
rerank.api_key_env = OPENROUTER_API_KEY
rerank.base_url = https://openrouter.ai/api/v1
rerank.input_k = 40
rerank.fail_open = false
```

理由：历史 benchmark、旧命令和当前 baseline 不应因为引入新能力而改变结果。
虽然 provider/model/api key 环境变量有默认值，但只要 `rerank.enabled = false`，
检索流程就不会访问 OpenRouter。

### 6.2 跑分默认

rerank 版本的 benchmark 应显式开启：

```text
rerank.enabled = true
candidate_k = 100
rerank.input_k = 40
```

最终 `top_k` 分场景处理：

- retrieval metrics：按 benchmark 要求返回。例如 Hit@10 就返回 10，Hit@30 就返
  回 30，不能强行压到 5。
- answer generation：按回答上下文预算返回。第一版建议 `answer_top_k = 5`，目
  标是减少 prompt 噪声。

### 6.3 交互式或产品场景

如果是低延迟交互场景，可以用更保守的参数：

```text
candidate_k = 40 or 80
rerank.input_k = 20 or 40
top_k = 5
fail_open = true
timeout_ms = 1000-3000
```

如果是离线评估：

```text
candidate_k = 100
rerank.input_k = 40
top_k = metric-dependent
fail_open = false
timeout_ms = generous or disabled
```

## 7. RAM-A 实现设计

### 7.1 配置结构

在 `RetrievalConfig` 中增加可选 rerank 配置：

```rust
pub struct RerankConfig {
    pub enabled: bool,
    pub provider: RerankProvider,
    pub model: String,
    pub api_key_env: String,
    pub base_url: String,
    pub input_k: usize,
    pub timeout_ms: Option<u64>,
    pub fail_open: bool,
}

pub enum RerankProvider {
    OpenRouter,
}
```

参数命名必须明确：

```text
candidate_k: dense 和 BM25 各自召回多少候选。
rerank_input_k: hybrid 融合后送入 reranker 的候选数。
top_k: 最终返回给调用方的结果数。
```

### 7.2 Reranker 接口边界

增加 provider-neutral trait：

```rust
#[async_trait]
pub trait Reranker: Send + Sync {
    async fn rerank(
        &self,
        query: &str,
        candidates: Vec<ScoredMemory>,
        top_k: usize,
    ) -> MemoryResult<Vec<ScoredMemory>>;
}
```

第一版实现 OpenRouter HTTP reranker client，而不是直接在 Rust 内嵌模型推理。
这里的含义是：RAM-A 的 Rust 代码只负责把 query 和候选 memory 文本通过 HTTP
发给 OpenRouter，由 OpenRouter 托管的 rerank 模型打分。这样不需要在 Rust 里
集成 libtorch、ONNX Runtime 或 HuggingFace tokenizer/model runtime。

选择 OpenRouter 的理由：

- Rust 保持检索管线清晰；
- 不需要维护本地 GPU/CPU 推理环境；
- 可以直接使用已有 `OPENROUTER_API_KEY`；
- benchmark 可以用同一个托管模型复现；
- 后续如需换 Cohere/Jina/Voyage 或本地服务，只要增加 provider，不需要重写检索
  主链路。

OpenRouter Rerank API request 形态：

```json
{
  "model": "cohere/rerank-v3.5",
  "query": "用户现在更喜欢喝什么咖啡？",
  "documents": [
    "用户以前常点拿铁。",
    "用户现在更喜欢手冲咖啡。"
  ],
  "top_n": 5
}
```

OpenRouter Rerank API response 形态：

```json
{
  "results": [
    {"index": 1, "relevance_score": 0.91},
    {"index": 0, "relevance_score": 0.34}
  ]
}
```

实现上用 `index` 映射回原始 `ScoredMemory`，并把最终 `score` 设置为
`relevance_score`。

参考：

- https://openrouter.ai/docs/api-reference/rerank
- https://openrouter.ai/openapi.json

### 7.3 Search flow

hybrid search 应拆成两个截断点：

```text
1. dense top candidate_k
2. BM25 top candidate_k
3. union candidates
4. hybrid score 排序
5. 如果 rerank 开启，截断到 rerank_input_k = 40
6. learned rerank 到最终 top_k
7. 如果 rerank 关闭，直接按 hybrid score 截断到 top_k
```

这样避免把完整 dense/BM25 union 都送给模型。

### 7.4 score 字段

第一版建议：

```text
rerank 关闭：score = hybrid score 或 dense/BM25 score，保持现状。
rerank 开启：score = rerank_score。
```

同时在未来报告或 debug 输出中补充：

```text
hybrid_score
rerank_score
dense_score
bm25_score
```

当前 `ScoredMemory` 只有一个 `score` 字段。第一版为了控制改动面，可以先让
`score` 表示最终排序分数。后续如果要做 explain/report，再扩展结构化 score
details。

### 7.5 失败策略

第一版区分两种模式：

```text
benchmark / evaluation: fail_closed
interactive / product: 可选 fail_open
```

`fail_closed` 表示 reranker 失败时整个 search 失败。这样 benchmark 不会悄悄退
化成 hybrid，还把结果标成 rerank。

`fail_open` 表示 reranker 超时或不可用时返回 hybrid 顺序。这适合交互式产品，
但必须在日志或 metadata 里暴露 fallback。

## 8. CLI 设计

建议新增参数：

```text
--rerank
--rerank-provider openrouter
--rerank-model cohere/rerank-v3.5
--rerank-api-key-env OPENROUTER_API_KEY
--rerank-base-url https://openrouter.ai/api/v1
--rerank-input-k 40
--rerank-timeout-ms
--rerank-fail-open
```

默认行为：

```text
不传 --rerank：完全保持当前 hybrid 行为，不读取 API key，不发 HTTP 请求。
传 --rerank：使用 OpenRouter provider，并从 --rerank-api-key-env 指定的环境变量读取 API key。
benchmark 默认 fail closed；如传 --rerank-fail-open，则 reranker 失败时回退到 hybrid 顺序。
```

benchmark 脚本应显式写出 rerank 参数，避免结果难以复现。

## 9. 评估计划

第一轮比较：

```text
baseline: current hybrid
rerank_40: hybrid -> rerank top 40 -> final top_k
```

后续如果需要做候选数对照，再加：

```text
rerank_20
rerank_80
```

必须记录：

- retrieval Hit@K 和 MRR；
- answer accuracy / judge score；
- average context tokens；
- p50 / p95 search latency；
- p50 / p95 rerank latency；
- reranker timeout / failure count；
- candidate_count；
- rerank_input_count；
- returned_count。

推荐接受条件：

```text
检索指标提升或至少不明显下降。
回答准确率提升，或上下文噪声下降。
rerank 延迟在离线跑分中可接受。
rerank 失败不会被静默隐藏。
```

## 10. 已确认的第一版决策

已确认按下面方案实现：

1. 第一版只做 OpenRouter HTTP reranker client，不在 Rust 里直接跑 HuggingFace
   模型。
2. 默认关闭 rerank，保持当前 dense/BM25/hybrid baseline 不变。
3. `rerank_input_k = 40`。
4. benchmark / evaluation 默认 fail closed。reranker 超时、401/429/5xx、API key
   缺失或响应格式异常时，本次 search 返回错误；只有显式传
   `--rerank-fail-open` 才 fallback 到 hybrid 顺序。
5. rerank 开启时，`ScoredMemory.score` 第一版直接使用 `rerank_score`。
6. 第一版不扩展 `ScoredMemory` 的 score details。score details 未来用于
   explain/report/debug，例如同时展示 `dense_score`、`bm25_score`、
   `hybrid_score`、`rerank_score`，帮助判断是召回阶段的问题还是 rerank 阶段
   的问题。
7. `memory-bench` 第一版只加 CLI 参数和 Rust benchmark 接入，不先改 Python
   evaluation wrapper 的默认参数。

## 11. 第一版落地建议

第一版实现目标：

```text
1. 保留当前 hybrid 默认行为。
2. 增加 RerankConfig，默认 disabled，input_k 默认 40。
3. 增加 Reranker trait。
4. 增加 OpenRouter HTTP reranker client。
5. hybrid search 在 rerank enabled 时：
   - 先按当前 hybrid score 排序；
   - 截断到 40；
   - 调 HTTP reranker；
   - 返回最终 top_k。
6. memory-bench 增加 --rerank 相关 CLI 参数。
7. 单元测试覆盖：
   - 默认关闭时结果保持 hybrid；
   - 开启 rerank 后 fake reranker 能改变排序；
   - rerank_input_k = 40 生效；
   - fail_closed / fail_open 行为。
```

第一版 benchmark 命令建议：

```text
--candidate-k 100
--rerank
--rerank-provider openrouter
--rerank-model cohere/rerank-v3.5
--rerank-api-key-env OPENROUTER_API_KEY
--rerank-input-k 40
```

回答阶段如果目标是降低上下文噪声，建议使用：

```text
answer_top_k = 5
```

但 retrieval metrics 的 `top_k` 应按 benchmark 定义，不要因为回答上下文选择 5
而影响 Hit@10、Hit@30 等指标。
