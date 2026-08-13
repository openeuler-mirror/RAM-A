# RAM-A 记忆流水线 Roadmap

本文定义 RAM-A 记忆流水线的演进方向，覆盖 conversation chunking、semantic
memory extraction 和 timeline-aware memory reasoning。SQLite 存储和
dense/BM25/hybrid 检索由 [sqlite-hybrid-search.md](sqlite-hybrid-search.md)
单独说明。

## 1. 范围

SQLite hybrid search 解决的是本地存储和候选检索问题：RAM-A 如何保存记忆记
录，以及如何用 dense embedding、BM25 或加权 hybrid ranker 检索候选记忆。

记忆流水线覆盖检索之前和检索之后的能力：

- 原始长对话如何变成稳定的记忆单元；
- 原始对话如何压缩成可长期保存的 durable memory；
- 用户偏好、事实和事件随着时间变化时如何表示；
- 检索和回答生成如何尊重最新状态和时间约束。

这些能力可以继续使用 SQLite store，但它们属于流水线层，而不是存储后端层。

## 2. 设计原则

1. 除非 benchmark owner 明确同意，否则保持 benchmark 评分标准和输出 schema
   稳定。
2. `memory-core` 保持 benchmark 无关。数据集原始字段应该留在
   `evaluation/<dataset>/` 适配器里处理。
3. 保留 provenance，也就是从生成后的记忆回溯到原始消息、chunk、时间戳和说话
   人的来源信息。
4. extraction 和 temporal reasoning 第一阶段都做成可选流水线阶段，让当前
   add/search baseline 仍然能跑。
5. 第一阶段优先扩展 metadata，避免破坏 `MemoryRecord` 和
   `benchmark-prepared-v1` 的现有消费者。
6. 流水线阶段要先按产品能力来设计，再用 benchmark adapter 做第一条验证路径。
   除非 benchmark 本身要求特殊格式，否则避免只服务 benchmark 的转换逻辑。

## 3. 第一层：Conversation Chunking

目标：在写入记忆库之前，把长对话切成更稳定的记忆 chunk。这样可以降低检索噪
声，让每个记忆单元更适合召回和重排。

本节概述边界策略和三套 benchmark 的落地路径；已经实现的 episode/window 行为
以 Rust `memory-pipeline` 的测试和公开 CLI 为准。

初始 chunk 边界应该支持：

- `speaker`：在说话人或角色重要时，按说话人/角色组合或切分；
- 时间窗口：把时间上接近的事件放在一起，避免跨越很大的时间间隔；
- 话题：当对话明显换话题时切开；
- token 长度：作为安全上限，避免 chunk 太长影响模型和检索稳定性。

预期输出示例：

```json
{
  "id": "conversation-7:chunk-4",
  "text": "User discussed preferring quiet vegan restaurants near work...",
  "metadata": {
    "scope_id": "conversation-7",
    "chunk_id": "conversation-7:chunk-4",
    "source_turn_ids": ["turn-21", "turn-22", "turn-23"],
    "speakers": ["user", "assistant"],
    "start_time": "2024-05-01T10:03:00Z",
    "end_time": "2024-05-01T10:08:00Z",
    "topic": "restaurants",
    "token_count": 184
  }
}
```

第一版实现应该是 deterministic，也就是同样输入永远得到同样输出，并且可以用
小 fixture 做测试。基于 LLM 的话题切分可以后置，等规则版 chunking 有稳定接口
后再加。

### 3.1 第一版 Chunking 设计

Chunking 不是 benchmark 专用的预处理技巧。它应该是原始对话类输入进入记忆系
统时的第一个 ingest 阶段：

```text
原始对话 / 聊天历史 / 导入 transcript
-> 规范化消息序列
-> conversation chunker
-> raw_chunk memories
-> retrieval / extraction / temporal rerank
```

如果调用方已经提供一条原子记忆，比如“用户喜欢燕麦奶拿铁”，chunker 不应该再
切它。Chunking 只适用于这些情况：

- 原始单位太短，单独看不懂；
- 原始单位太长，检索时容易带入太多噪声；
- 原始单位混杂了邻近上下文，需要整理成更稳定的记忆块。

因此第一版应该暴露一个通用 chunker，输入是规范化后的 message sequence。
PersonaMem、LongMemEval 和 LoCoMo 的 adapter 只负责把各自数据集记录翻译成这
个通用输入：

```json
{
  "source_kind": "conversation",
  "scope_id": "conversation-7",
  "messages": [
    {
      "id": "turn-21",
      "speaker": "user",
      "role": "user",
      "text": "I used to enjoy writing album reviews.",
      "timestamp": "2024-05-01T10:03:00Z",
      "metadata": {"turn_index": 21}
    }
  ]
}
```

chunker 输出的记录可以通过现有 `benchmark-prepared-v1` memory 形态存储；以后
也可以接到产品 ingest API：

```json
{
  "id": "conversation-7:chunk:0004",
  "text": "User used to enjoy writing album reviews...\nUser later felt...",
  "metadata": {
    "scope_id": "conversation-7",
    "memory_kind": "raw_chunk",
    "chunk_id": "conversation-7:chunk:0004",
    "source_message_ids": ["turn-21", "turn-22", "turn-23"],
    "source_paths": ["$.memories[21].text", "$.memories[22].text"],
    "speakers": ["user"],
    "roles": ["user"],
    "start_time": "2024-05-01T10:03:00Z",
    "end_time": "2024-05-01T10:08:00Z",
    "start_index": 21,
    "end_index": 23,
    "token_count": 236
  }
}
```

### 3.2 边界策略

token 数量应该是安全护栏，不应该成为 chunk 的主要定义。真实记忆的大小不一：
生日这种事实很短，偏好演变可能需要几轮对话，多人事件可能需要足够的前后文才
能看懂。因此 chunker 应该优先根据“是否出现了新的记忆单元”来判断边界，再用
token 上限防止 chunk 异常过长。

边界信号按下面顺序判断：

1. 硬边界：`scope_id` 变化；
2. 硬边界：session/document 变化，前提是数据集或产品来源暴露了 session；
3. 强边界：时间间隔超过配置阈值；
4. 强边界：有明确 topic marker，并且 topic 变化；
5. 中等边界：speaker/role 模式变化，表示新一轮 exchange 开始，例如用户从偏好
   更新转向一个新问题；
6. 弱边界：文本提示出现新的事件或状态，例如 “later”、“now”、“however”、
   “after that”、“I changed my mind”、“new topic”；
7. 安全边界：加入下一条消息会超过 `max_tokens`。

第一版可以用确定性的规则实现：

```text
开始一个 chunk
如果下一条消息仍属于同一个局部事件/话题，就追加进去
遇到硬边界或强边界时切开
超过 max_tokens 前切开
只有 token-limit 导致切分时，才允许很小的一条消息重叠
```

推荐初始参数：

```text
target_tokens = 200-350
max_tokens = 600
min_tokens = 40
overlap_turns = 1 only for token-limit splits
timestamp_gap_minutes = dataset/source specific
```

`target_tokens` 只是告诉 chunker：如果附近有合适边界，可以考虑切开。
`max_tokens` 才是硬上限，用来防止 chunk 无限制变长。当一个 chunk 已经完整表达
一个原子事实或事件时，它可以短于 `target_tokens`。

### 3.3 例子

原始 turns：

```text
[1] User: I used to enjoy writing album reviews.
[2] User: I liked having a place to express how music made me feel.
[3] User: Later, people kept saying my reviews were not objective.
[4] User: That pressure made me second-guess my connection to the music.
[5] User: Now I would rather express that interest privately.
[6] User: Separately, I am planning a weekend trip.
```

如果一条 turn 就是一条 memory，检索会很脆弱：

```text
"Now I would rather express that interest privately."
```

这句话没有说明 “that interest” 指什么，也没有说明用户为什么改变。相反，如果把
所有内容塞成一个过大的 chunk，又可能把周末旅行这种无关内容带进回答模型。

更理想的 chunking：

```text
chunk A:
User used to enjoy writing album reviews because they helped express feelings
about music. Later, criticism about objectivity created pressure, so the user
now prefers expressing that music interest privately.

chunk B:
User is planning a weekend trip.
```

这样可以把一次偏好演变事件放在一起，同时把无关旅行话题切开。它也给后续
extraction 足够上下文，方便抽取这些 state update：

```text
old preference: enjoyed writing album reviews
reason for change: criticism and pressure
current preference: private music expression
```

### 3.4 数据集验证路径

第一条验证路径应该使用三类 benchmark，因为它们覆盖了比较真实的输入形态；但实
现本身仍然要保持通用。

- PersonaMem：把 shared-context messages 转成规范化消息序列，按
  `shared_context_id` 分组；重点验证 preference evolution 和 recommendation 类
  问题。
- LongMemEval：把 haystack sessions 转成规范化消息序列，按 question scope 分
  组；保留 session IDs、turn IDs、dates 和 `has_answer` provenance，用于检索
  诊断。
- LoCoMo：把每个 conversation 的 sessions 转成规范化消息序列，按
  conversation/sample 分组；保留 speaker、session、timestamp 和原始 turn path，
  这样 evidence-hit 指标仍然能工作。

adapter 可以同时产出 raw prepared 文件和 chunked prepared 文件：

```text
personalmem_32k_v1.json
personalmem_32k_v1_chunked.json
```

现有 raw prepared 输出必须保留，这样 baseline run 才能继续公平对比。

### 3.5 验收标准

第一阶段 chunking 的验收重点应该是稳定性和诊断价值，不应该一开始就承诺大幅涨
分：

1. 同样输入能产生 byte-stable 的 chunked 输出；
2. chunk 不跨 `scope_id`；
3. chunk 不丢失 source provenance；
4. chunked prepared 文件仍能通过 `memory-bench add/search`；
5. PersonaMem、LongMemEval 和 LoCoMo smoke run 能跑通；
6. answer context token usage 不高于 raw-memory baseline；
7. 加 extraction 之前，QA accuracy 不出现明显回退；
8. error report 能把被检索到的 chunk 映射回 source messages 或 turns。

如果 chunking 降低了上下文噪声，或者提升了 preference evolution、
multi-session reasoning、temporal reasoning 等类别，就把它作为 semantic memory
extraction 的输入格式。如果它伤害了准确率，就保持 opt-in，并检查是哪条边界规
则把有用证据切散了。

## 4. 第二层：Semantic Memory Extraction

目标：不要只存原始对话。RAM-A 应该从 chunk 中抽取结构化长期记忆候选，然后存
成更简短的 memories，并保留足够 metadata 支持检索、重排和答案溯源。

初始 memory types：

- `preference`：用户喜欢、不喜欢、习惯、约束和目标；
- `fact`：稳定的用户事实或环境事实；
- `relationship`：人物、组织和关系上下文；
- `event`：在某个时间点或时间段发生过的事情；
- `state_update`：偏好、计划、状态或可用性的变化。

预期输出示例：

```json
{
  "id": "conversation-7:chunk-4:memory-2",
  "text": "User prefers quiet vegan restaurants near work.",
  "metadata": {
    "scope_id": "conversation-7",
    "memory_type": "preference",
    "subject": "user",
    "predicate": "prefers",
    "object": "quiet vegan restaurants near work",
    "source_chunk_id": "conversation-7:chunk-4",
    "confidence": 0.86
  }
}
```

extraction 阶段应使用 RAM-A 已有的 OpenAI-compatible LLM client，以及 LoCoMo
judge 调用中已经引入的本地 JSON 解析约定。在跑真实 API benchmark 前，应先用
stubbed model response 做单元测试。

### 4.1 运行时归属

Episode、window、extraction、validation、grounding、deduplication 和 prepared
output 已迁移到 Rust `memory-pipeline` crate。Python 只保留数据集 adapter、评测
编排、指标和报告；原 Python memory pipeline 及其行为对照测试已删除，离线回归
通过 fixture 直接验证 Rust CLI。MCP 接入不属于本次迁移范围。

## 5. 第三层：Timeline-Aware Memory Reasoning

目标：用能处理偏好演变、最新状态和时间约束问题的方式表示和检索记忆。

核心 metadata：

- `event_time`：被记住的事件实际发生时间；
- `observed_at`：RAM-A 获知这条记忆的时间；
- `valid_from` 和 `valid_to`：已知的有效时间区间；
- `supersedes`：被当前记忆替代或削弱的旧记忆 ID；
- `status`：`active`、`superseded`、`expired` 或 `uncertain`；
- `temporal_confidence`：时间解释的置信度。

目标场景：

- 用户以前喜欢 A，后来改成喜欢 B；
- 问题询问最新偏好或当前状态；
- 问题询问某个特定时间窗口内的情况；
- 多条记忆互相冲突，需要时间线重排。

初始检索策略：

1. 用现有 dense/BM25/hybrid search 检索候选记忆；
2. 当 query metadata 可用时，应用可选 temporal filters；
3. 将 active 和 recent state updates 排在 superseded records 前面；
4. 把 provenance 和 temporal metadata 暴露给 answer layer。

这套能力以后可以演进成更完整的 timeline-aware reasoning，但第一版应该保持足
够简单，能用 deterministic fixtures 测试。

## 6. 演进顺序

1. 增加通用 deterministic chunking 模块和 fixture tests。
2. 让 prepared dataset adapters 可以可选输出 chunked memories，同时保留当前
   benchmark 输出。
3. 增加 retrieval/answer reports，让报告能把 retrieved chunks 映射回 source
   messages。
4. 在显式 flag 后面增加 semantic extraction，并用 stubbed LLM responses 测试。
5. 用 metadata 扩展 temporal fields，不破坏现有 `MemoryRecord` consumers。
6. 增加 temporal rerank，作为 opt-in retrieval stage。
7. 在开启新 pipeline 做 full benchmark 前，先跑 PersonaMem 和 LoCoMo smoke
   comparisons。

## 7. 开放问题

- extracted memories 和 raw chunks 是否共用同一个 store，还是用
  `memory_kind = raw_chunk|extracted_memory` 区分？
- 第一版哪些 temporal fields 应该变成 Rust 一等结构，哪些继续放在 generic
  metadata 中？
- extraction 只在 add time 运行，还是 RAM-A 也应该支持 prompts 或 schema 变化
  后的离线 re-extraction？
- benchmark reports 应该如何对比 raw-chunk retrieval 和 extracted-memory
  retrieval，同时不改变现有评分 schema？

## 8. 统一 A/B 治理入口

PersonaMem、LongMemEval 和 LoCoMo 现在由
`evaluation/scripts/run_memory_ab.py` 按同一顺序运行：normal 模式直接执行 full
pair；strict 模式额外执行 policy 与 dataset-bound regression preflight，随后运行
fresh raw、extracted、comparison。Preflight hash 在 arm 启动前由各 runner 验证并写入
`config.json`；Python 不补写或伪造 Rust memory pipeline 的结果。只有完整 full pair
进入版本化 JSONL history，晋级失败的完整 pair 保留为 failed，不完整 pair 不写。
