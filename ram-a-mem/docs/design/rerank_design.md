# RAM-A Rerank

本文说明 RAM-A 的可选 rerank 阶段。Rerank 位于 hybrid retrieval 之后，用 learned
reranker 对候选记忆重新排序，再返回最终 `top_k`。

## 1. 功能范围

RAM-A 当前支持四类检索相关能力：

- `dense`：基于 embedding 相似度召回。
- `bm25`：基于 SQLite FTS5 的关键词召回。
- `hybrid`：融合 dense 和 BM25 候选。
- `rerank`：在 hybrid 融合后，对候选文本进行模型精排。

Rerank 默认关闭。关闭时，`hybrid` 仍按 dense/BM25 加权融合分数返回结果；开启
后，`hybrid` 会先融合出最多 `rerank.input_k` 条候选，再调用 reranker 得到最终
排序。

## 2. 检索流程

开启 rerank 后，`SearchMode::Hybrid` 的流程如下：

```text
query
-> query embedding
-> dense candidates from store
-> BM25 candidates from SQLite FTS
-> hybrid score fusion
-> truncate to rerank.input_k
-> reranker scores (query, memory.text)
-> final top_k
```

`candidate_k`、`rerank.input_k` 和 `top_k` 是三个独立参数：

| 参数 | 含义 |
| --- | --- |
| `candidate_k` | dense 和 BM25 各自召回的候选数量。 |
| `rerank.input_k` | hybrid 融合后送入 reranker 的候选数量。 |
| `top_k` | search 返回给调用方的最终结果数量。 |

如果 `rerank.input_k < top_k`，运行时会至少保留 `top_k` 条候选，避免 rerank 输入
数量小于最终返回数量。

## 3. 默认配置

`RerankConfig::default()`：

```text
enabled = false
provider = openrouter
model = cohere/rerank-v3.5
api_key_env = OPENROUTER_API_KEY
base_url = https://openrouter.ai/api/v1
input_k = 40
timeout_ms = None
fail_open = false
```

默认关闭的原因是保持现有 dense/BM25/hybrid baseline 可复现，并避免默认命令访问
外部 API。

## 4. Provider 接口

`memory-core` 暴露 provider-neutral trait：

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

当前 provider 为 `OpenRouterReranker`。客户端向 `{base_url}/rerank` 发起 HTTP
请求；如果 `base_url` 已经以 `/rerank` 结尾，则直接使用该 URL。

请求体：

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

响应体：

```json
{
  "results": [
    {"index": 1, "relevance_score": 0.91},
    {"index": 0, "relevance_score": 0.34}
  ]
}
```

实现会用 `index` 映射回原始 `ScoredMemory`，并将最终 `score` 设置为
`relevance_score`。响应校验包括：

- 返回数量不能超过 `top_n`。
- `index` 必须落在候选范围内。
- `index` 不能重复。
- `relevance_score` 必须是 finite number。

## 5. 失败策略

`fail_open = false` 时，reranker 失败会让本次 search 返回 `MemoryError::Rerank`。
这适合 benchmark 和离线评估，因为结果不会静默退化成普通 hybrid。

`fail_open = true` 时，reranker 失败会返回 hybrid 原始顺序并截断到 `top_k`。这适
合交互式场景，但调用方应在日志或报告中记录 fallback。

OpenRouter client 对网络错误、HTTP 429/5xx 和可重试服务错误做有限重试；认证、
额度和响应格式错误会直接返回错误。

## 6. CLI 使用

`memory-bench` 默认不启用 rerank。开启方式：

```bash
cargo run -p memory-bench -- \
  --store-backend sqlite \
  --store data/locomo/store.sqlite \
  --search-mode hybrid \
  --embedding openrouter \
  --rerank \
  --rerank-provider openrouter \
  --rerank-model cohere/rerank-v3.5 \
  --rerank-api-key-env OPENROUTER_API_KEY \
  --rerank-base-url https://openrouter.ai/api/v1 \
  --rerank-input-k 40 \
  search \
  --dataset data/locomo/prepared/locomo_v1.json \
  --top-k 10 \
  --output outputs/locomo_search_results.json
```

可选参数：

```text
--rerank-timeout-ms <MILLISECONDS>
--rerank-fail-open
```

评测脚本中，`evaluation/run_locomo_eval.sh` 支持通过环境变量启用 rerank：

```bash
RERANK=1 \
RERANK_MODEL=cohere/rerank-v3.5 \
RERANK_INPUT_K=40 \
OPENROUTER_API_KEY=... \
bash evaluation/run_locomo_eval.sh
```

## 7. 代码位置

| 路径 | 作用 |
| --- | --- |
| `crates/memory-core/src/api.rs` | `RerankConfig`、`RerankProvider` 和默认值。 |
| `crates/memory-core/src/rerank.rs` | `Reranker` trait 和 `OpenRouterReranker`。 |
| `crates/memory-core/src/manager.rs` | hybrid 后的 rerank 调用和 fail-open/fail-closed 行为。 |
| `crates/memory-bench/src/main.rs` | CLI 参数和 OpenRouter reranker 构建。 |
| `evaluation/run_locomo_eval.sh` | LoCoMo rerank 评测入口。 |
