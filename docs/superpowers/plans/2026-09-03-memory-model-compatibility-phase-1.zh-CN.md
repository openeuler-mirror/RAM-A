# RAM-A-MEM 模型兼容性第一阶段实施计划

> 本计划只覆盖个人记忆摄入管线的 Extract 与 Ground 两个 Chat Completions 调用。
> 实施时按任务逐项测试、提交；失败策略保持现状。

## 目标

在不引入完整多 Backend 工厂、不改变 `fail_fast` 与失败窗口处理语义的前提下，完成以下四项增强：

1. 增加类型化的 Provider 能力配置；
2. 识别只有 `reasoning_content`、没有最终 `content` 的响应，并进行至多一次纠正重试；
3. 支持可配置的 Structured Output，并对非空但不合法的业务 JSON 进行至多一次格式修复重试；
4. 将窗口、输出及上下文预算配置化，并在请求前进行预算检查。

同时更新所有相关配置示例、部署文档、验证脚本，并新增一篇独立的模型兼容性说明。

## 实施状态

本提交已实现 Task 1～4 的源码变更，并同步 Task 5～6 的配置样例、ARM64 资产测试和文档。
真实 GLM Provider smoke、ARM64 容器端到端验证和 RPM 验收仍属于外部环境验证项，不在本机
Docker 不可用时标记为完成。

## 明确不在本阶段修改的内容

- 不改变 `pipeline.fail_fast` 的默认值或含义；
- 不增加“失败后保存原始窗口”“生成占位记忆”等降级策略；
- 不把 `reasoning_content` 当作最终答案或业务 JSON；
- 不引入任意 JSON `extra_body` 透传，避免拼写错误、配置漂移和意外发送敏感字段；
- 不建立模型上下文窗口内置表，模型窗口由部署者显式配置；
- 不改 Embedding、Rerank、Case Summary 和 Graph Memory 的模型调用；
- 不在这一阶段实现完整的 OpenAI/Qwen/DeepSeek/Anthropic 多 Backend 工厂。

## 配置方案

保留现有 `providers.reasoning_effort`，新增字段均采用强类型和白名单枚举。建议配置形态如下：

```json
{
  "pipeline": {
    "fail_fast": true,
    "max_memory_chars": 500,
    "max_candidate_tokens": 320,
    "max_window_tokens": 640,
    "extractor_max_output_tokens": 1600,
    "verifier_max_output_tokens": 1000,
    "extractor_context_window_tokens": null,
    "verifier_context_window_tokens": null,
    "reasoning_reserve_tokens": 0
  },
  "providers": {
    "reasoning_effort": "none",
    "enable_thinking": null,
    "send_temperature": true,
    "temperature": 0.0,
    "output_token_parameter": "max_tokens",
    "structured_output": "prompt_only",
    "reasoning_only_retry": true,
    "json_repair_attempts": 1
  }
}
```

约束如下：

- `reasoning_effort` 与 `enable_thinking` 是两个已知协议字段，配置时最多启用一种；未配置就不发送；
- `send_temperature=false` 时完全省略 `temperature`；
- `output_token_parameter` 只允许 `max_tokens` 或 `max_completion_tokens`；
- `structured_output` 只允许 `prompt_only`、`json_object`、`json_schema`；默认使用 `prompt_only` 保持兼容，具体部署通过 smoke test 后再启用更强模式；
- `reasoning_only_retry` 只控制一次语义纠正重试，不占用或混淆现有网络/HTTP 重试次数；
- `json_repair_attempts` 第一阶段只允许 `0` 或 `1`；
- 上下文窗口为 `null` 时保留当前行为，只应用窗口及输出上限，不做总上下文硬检查；
- 默认值保持当前生产行为；ARM64 GLM 部署配置显式开启已经验证支持的选项。

## Task 1：Provider 能力配置和请求构造

**主要文件：**

- 修改 `ram-a-mem/crates/memory-pipeline/src/client.rs`
- 修改 `ram-a-mem/crates/memory-mcp/src/config.rs`
- 修改 `ram-a-mem/crates/memory-mcp/src/main.rs`
- 修改 `ram-a-mem/crates/memory-mcp/tests/http_mcp.rs`
- 视构造器兼容需要修改 `ram-a-mem/crates/memory-pipeline/src/main.rs`

**实施步骤：**

- [ ] 在 `memory-pipeline` 中定义 `ChatCompatibilityOptions`、`OutputTokenParameter` 和 `StructuredOutputMode` 等强类型；
- [ ] 让 `OpenAiCompatibleClient` 统一构造请求体，按配置发送或省略 `reasoning_effort`、`enable_thinking`、`temperature` 和输出 token 字段；
- [ ] Structured Output 的具体 JSON Schema 由 Extract/Ground 调用方传入，客户端只负责按模式编码 `response_format`；
- [ ] 在 `ProvidersConfig` 中增加字段、默认值和边界校验，继续使用 `deny_unknown_fields`；
- [ ] 校验互斥 thinking 控制、有限枚举、有限修复次数、有限温度范围及非空字符串；
- [ ] 将配置一次性注入 Extract/Ground 共用的客户端，避免两个阶段产生配置漂移；
- [ ] 将兼容配置纳入组件/cache identity，保证请求协议或预算变化后不会命中旧缓存结果。

**测试：**

- [ ] 本地捕获服务器断言每个可选字段的发送、缺省和互斥行为；
- [ ] 分别断言 `max_tokens` 与 `max_completion_tokens` 的请求形态；
- [ ] 分别断言 `prompt_only`、`json_object` 与 `json_schema` 的 `response_format`；
- [ ] 配置反序列化测试覆盖默认值、完整配置和非法组合；
- [ ] 回归现有 `reasoning_effort="none"` 请求测试。

## Task 2：reasoning-only 检测与一次纠正重试

**主要文件：**

- 修改 `ram-a-mem/crates/memory-pipeline/src/client.rs`
- 修改 `ram-a-mem/crates/memory-pipeline/src/error.rs`（仅在需要稳定错误分类时）
- 修改 `ram-a-mem/crates/memory-pipeline/src/observability.rs` 或现有日志测试位置

**实施步骤：**

- [ ] 将响应解析扩展为读取 `content`、`reasoning_content`、`finish_reason` 和 usage，但不保存、打印或返回原始 reasoning 文本；
- [ ] 区分 `empty_content` 与 `reasoning_only`：只有 reasoning 非空且最终 content 为空时才标记为后者；
- [ ] 当 `reasoning_only_retry=true` 时执行至多一次纠正重试；纠正请求使用已配置的关闭 thinking 能力和该阶段既定输出预算；
- [ ] 如果请求已经明确关闭 thinking，或 Provider 没有可用的关闭参数，仍只允许一次有界重试，最终返回稳定的 `reasoning_only` 协议错误；
- [ ] 网络/HTTP 指数退避仍由现有 `max_retries` 控制，语义纠正重试单独计数，避免次数相乘失控；
- [ ] 新增结构化日志事件，例如 `ram_a.provider.reasoning_only`，仅记录模型、阶段、尝试次数、`finish_reason` 和 token usage，不记录用户正文或 reasoning 正文。

**测试：**

- [ ] 第一次 reasoning-only、第二次返回 content 时成功；
- [ ] 两次均 reasoning-only 时返回稳定错误且不继续重试；
- [ ] content 与 reasoning 同时存在时使用 content，不触发重试；
- [ ] 两者都为空时保持 `empty_content` 分类；
- [ ] 日志测试确认不泄露 reasoning 文本或 API 响应正文；
- [ ] 验证语义重试次数与 transport retry 次数互不污染。

## Task 3：Structured Output 与 JSON 格式修复重试

**主要文件：**

- 修改 `ram-a-mem/crates/memory-pipeline/src/extraction.rs`
- 修改 `ram-a-mem/crates/memory-pipeline/src/grounding.rs`
- 修改 `ram-a-mem/crates/memory-pipeline/src/client.rs`
- 可参考但不直接耦合 `ram-a-mem/crates/memory-core/src/graph/llm.rs`

**实施步骤：**

- [ ] 为 Extract 的 `atomic_memory_v1` 和 Ground 的判定结果分别定义稳定 JSON Schema；
- [ ] `json_schema` 模式发送严格 schema，`json_object` 模式只要求 JSON 对象，`prompt_only` 保持当前请求形态；
- [ ] 不做隐式模式降级；若 Provider 不支持某模式，启动 smoke test/部署配置必须显式切换，避免把其他 HTTP 400 错误误判为能力不足；
- [ ] 保留现有代码围栏清理，但将“非空 content 无法解析”“顶层类型错误”“缺失 schema_version/必填字段”归为可修复格式错误；
- [ ] 格式错误时由 Extract/Ground 各自执行至多一次 repair 调用，因为只有业务层知道目标 schema；
- [ ] repair 提示词携带受长度限制的原输出和精简错误信息，要求只修复格式、不得新增事实；
- [ ] 修复结果仍必须完整经过现有 schema 校验、Validation 和 Grounding，不绕过任何质量门；
- [ ] 业务语义错误、证据不匹配、HTTP 错误和空 content 不进入 JSON repair；
- [ ] repair 次数、结果与错误类型写入结构化日志，但不记录原始记忆内容。

**测试：**

- [ ] Extract/Ground 两套 schema 的请求体快照测试；
- [ ] 合法 JSON、代码围栏 JSON 不触发 repair；
- [ ] 带解释文字、截断 JSON 或错误顶层结构在一次 repair 后成功；
- [ ] repair 仍非法时立即返回原有失败路径，不进行第二次 repair；
- [ ] 非格式类错误不触发 repair；
- [ ] cache identity 包含 structured mode、repair policy、输出预算和 prompt/schema 版本。

## Task 4：输入、输出与上下文 token 预算配置化

**主要文件：**

- 修改 `ram-a-mem/crates/memory-pipeline/src/window.rs`
- 修改 `ram-a-mem/crates/memory-pipeline/src/extraction.rs`
- 修改 `ram-a-mem/crates/memory-pipeline/src/grounding.rs`
- 修改 `ram-a-mem/crates/memory-pipeline/src/pipeline.rs`
- 修改 `ram-a-mem/crates/memory-mcp/src/config.rs`
- 修改 `ram-a-mem/crates/memory-mcp/src/main.rs`

**实施步骤：**

- [ ] 将现有硬编码默认值原样提升到配置：candidate 320、window 640、Extract 输出 1600、Ground 输出 1000；
- [ ] 为 Extract/Ground 分别增加可选的模型上下文窗口，并增加共享 reasoning reserve；
- [ ] 启动时校验 `max_candidate_tokens <= max_window_tokens`、各预算大于零、上下文窗口能容纳最小固定开销；
- [ ] 请求前估算 `system prompt + rendered user prompt + reasoning reserve + max output`；
- [ ] 若超预算，优先缩减外围上下文消息，不截断候选消息及 evidence span；
- [ ] 如果固定提示词、候选内容和保留输出预算仍无法放入窗口，返回稳定的 `context_budget_exceeded` 错误；随后仍由现有 `fail_fast` 决定管线是否终止，失败策略本身不变；
- [ ] 文档明确当前 estimator 是启发式估算，不等同于目标模型 tokenizer，也不保证服务端一定接受；
- [ ] usage 日志保留 Provider 返回值，缺失 usage 时继续使用本地估算。

**测试：**

- [ ] 配置默认值与当前行为一致；
- [ ] 非法预算组合在启动时被拒绝；
- [ ] 动态预算足够时不删上下文；
- [ ] 超预算时只缩减外围上下文；
- [ ] 候选本体无法容纳时返回 `context_budget_exceeded`；
- [ ] `fail_fast=true/false` 现有测试保持不变并全部通过；
- [ ] 使用中文、英文和混合标点输入覆盖现有启发式 token estimator。

## Task 5：配置样例、部署资产和验证脚本同步

**主要文件：**

- 修改 `ram-a-mem/plugins/mcp/ram-a-mem.json`
- 修改 `deploy/arm64/config/ram-a-mem.json`
- 修改 `deploy/arm64/tests/test_assets.py`
- 修改 `deploy/arm64/scripts/verify-ingest.sh`
- 按实际字段支持情况检查 `deploy/arm64/config/xiaoo.toml`，不混淆 xiaoO 与 RAM-A 的请求配置

**实施步骤：**

- [ ] 通用样例显式展示安全默认值和注释说明；
- [ ] ARM64 GLM 配置保留 `reasoning_effort="none"`，其余模式只启用 live smoke 已确认支持的值；
- [ ] smoke test 分别验证最终 `content`、reasoning-only 分类、Structured Output 模式和足够的输出预算；
- [ ] 摄入 smoke test 继续验证 accepted count、memory IDs、检索命中和持久化，不把“HTTP 200”当成成功；
- [ ] 资产测试断言配置字段、脚本参数和文档样例同步，防止镜像内容漂移；
- [ ] 密钥仍只通过环境变量解析，测试输出和镜像日志不得打印密钥。

## Task 6：文档同步与模型兼容性说明

**新增文档：**

- 新增 `ram-a-mem/docs/guides/model-compatibility.zh-CN.md`

**修改文档：**

- 修改 `ram-a-mem/docs/README.md`
- 修改 `ram-a-mem/README.md`
- 修改 `ram-a-mem/docs/guides/ram-a-mem-configuration-reference.zh-CN.md`
- 修改 `ram-a-mem/docs/guides/ram-a-mem-configuration-and-pipeline.zh-CN.md`
- 修改 `ram-a-mem/docs/guides/http-mcp-deployment.md`
- 修改 `ram-a-mem/docs/guides/ram-a-mem-rpm-agent-self-test.zh-CN.md`
- 在 `docs/superpowers/specs/2026-09-02-provider-reasoning-effort-design.md` 增加后续设计链接，不改写历史结论

**模型兼容性文档必须说明：**

- RAM-A 所谓 OpenAI-compatible 的精确协议契约：URL、鉴权、messages、最终 `choices[0].message.content`、usage；
- RAM-A 不要求模型天然属于“推理”或“非推理”类别，要求的是稳定产出最终 content 和目标 JSON；
- `reasoning_content` 只用于响应分类，绝不会作为记忆结果解析；
- `reasoning_effort`、`enable_thinking` 的适用边界、互斥规则和 Provider 不支持时的处理；
- `prompt_only`、`json_object`、`json_schema` 三种模式的能力要求和选择顺序；
- `max_tokens`/`max_completion_tokens` 的单位是输出 token，不是字符、字数或总上下文；
- 完整预算公式、启发式 estimator 限制、上下文窗口配置责任；
- transport retry、reasoning-only correction、JSON repair 三类重试的区别和最大次数；
- GLM Coding Plan 的已验证配置示例，以及“短输出预算可能只得到 reasoning、没有 content”的已知现象；
- 通用 OpenAI-compatible 与支持 `enable_thinking` 服务的示例，但不宣称未实测模型已兼容；
- 上线前 smoke test 清单、可观测事件、错误码及敏感信息保护；
- 当前不支持的边界：混在 content 中的思维链清理、任意 Provider 私有协议、自动模型窗口识别、自动 Structured Output 降级。

## Task 7：整体回归与交付

- [ ] 运行 `cargo fmt --all -- --check`；
- [ ] 运行 `cargo test -p memory-pipeline`；
- [ ] 运行 `cargo test -p memory-mcp`；
- [ ] 运行 ARM64 资产测试；
- [ ] 对 GLM 做协议级 smoke：thinking 关闭字段、Structured Output、reasoning-only 响应和 token usage；
- [ ] 在容器中执行完整 `memory_ingest -> memory_search -> restart -> memory_search`；
- [ ] 检查结构化日志不包含 API key、原始 reasoning 或完整 Provider 响应；
- [ ] 审阅配置兼容性、文档链接和缓存身份；
- [ ] 按 Task 1～6 拆分为可审阅提交，最终推送到 PR18 原分支。

## 验收标准

1. GLM 返回 reasoning 与 content 时，RAM-A 只解析 content；只有 reasoning 时只发生一次有界纠正重试；
2. 支持显式选择三种结构化输出模式，不支持的 Provider 可通过配置回到 `prompt_only`；
3. 非空但格式错误的业务 JSON 最多修复一次，修复后仍通过全部现有质量门；
4. 四类核心 token 预算可配置，并能在请求前发现明确的上下文超限；
5. `fail_fast`、rejected/quarantined 计数和失败窗口语义与本阶段开始前一致；
6. 通用配置、ARM 镜像配置、验证脚本、README、配置参考、管线文档及新增兼容性文档保持一致；
7. 单元测试、集成测试和容器内真实摄入验证全部通过后才推送。
