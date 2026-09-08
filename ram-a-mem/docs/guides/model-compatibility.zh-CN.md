# RAM-A-MEM 记忆抽取模型兼容性

本文只描述个人记忆摄入管线的 Extract 和 Ground 阶段。Embedding、Rerank、Case
Summary 与 Graph Memory 使用各自的客户端和配置，不自动继承本文选项。

## 兼容协议

RAM-A 不要求模型必须是“推理模型”或“非推理模型”。兼容性的实际要求是：

1. 服务接受 Bearer 鉴权的 OpenAI-compatible `POST /chat/completions`；
2. 接受 `model`、`messages` 以及配置选定的输出 token 字段；
3. 最终答案稳定返回到 `choices[0].message.content`；
4. Extract 能返回 `atomic_memory_v1` JSON，Ground 能返回 `results` JSON。

`choices[0].message.reasoning_content` 可以存在，但 RAM-A 不解析、不落盘，也不会把它当作
最终答案。如果 reasoning 非空而 content 为空，响应分类为 `reasoning_only`。启用
`reasoning_only_retry` 后，RAM-A 会至多纠正重试一次：请求会再次强调必须返回最终 content，
并通过已配置的 thinking 控制字段要求关闭推理。第二次仍无 content 就返回协议错误。

## Provider 能力配置

```json
{
  "reasoning_effort": "none",
  "enable_thinking": null,
  "send_temperature": true,
  "temperature": 0.0,
  "output_token_parameter": "max_tokens",
  "structured_output": "prompt_only",
  "reasoning_only_retry": true,
  "json_repair_attempts": 1
}
```

- `reasoning_effort`：适用于接受该字段的服务，例如已验证的 GLM Coding Plan。`none`
  表示请求关闭/尽量关闭推理；服务是否严格执行仍由服务端决定。
- `enable_thinking`：适用于明确接受布尔字段的服务。它与 `reasoning_effort` 不能同时配置。
- `send_temperature=false`：完全省略 `temperature`，用于拒绝该参数的模型。
- `output_token_parameter`：只允许 `max_tokens` 或 `max_completion_tokens`。
- `structured_output`：可选 `prompt_only`、`json_object`、`json_schema`。不支持更强模式的
  Provider 应显式回到 `prompt_only`；RAM-A 不会把任意 HTTP 400 自动解释为能力不足。
- `json_repair_attempts`：只允许 0 或 1。非空 content 的 JSON 语法或 schema envelope
  错误可以修复一次；HTTP 错误、空 content 和证据不一致不进入 repair。

没有任意 `extra_body` 透传。新增私有 Provider 参数前，应先在强类型配置中定义语义、边界
和脱敏测试。

## Structured Output 和 JSON repair

`prompt_only` 只依赖提示词；兼容范围最大、约束最弱。`json_object` 发送
`response_format={"type":"json_object"}`。`json_schema` 为 Extract 和 Ground 分别发送
命名 schema：Ground 的 schema 完全封闭（全字段 `required` +
`additionalProperties: false`），以 `strict: true` 发送；Extract 的 schema 因
`attributes` 允许任意键、部分字段可选，无法满足 OpenAI strict 硬规则，以
`strict: false` 发送并依赖宽松实现，解析后仍由 `validate_extraction` 完整校验。

格式修复由业务阶段发起，而不是 HTTP 客户端发起，因为只有 Extract/Ground 知道目标
schema。repair 请求只允许修复格式，不得新增事实；结果仍完整经过 schema 校验、Validation
与 Grounding，不绕过任何质量门。transport retry、reasoning-only correction 和 JSON repair
是三套独立且有界的机制。

## Token 预算

`max_tokens` 和 `max_completion_tokens` 的单位都是输出 token，不是字符、汉字个数，也不是
模型总上下文。服务配置可分别控制：

- `max_candidate_tokens`：单个候选窗口预算，默认 320；
- `max_window_tokens`：候选加外围上下文预算，默认 640；
- `extractor_max_output_tokens`：Extract 最大输出，默认 1600；
- `verifier_max_output_tokens`：Ground 最大输出，默认 1000；
- `extractor_context_window_tokens`、`verifier_context_window_tokens`：可选的模型总窗口；
- `reasoning_reserve_tokens`：为可能存在的推理预留的 token。

配置总窗口后，请求前检查近似关系：

```text
估算的 messages token + reasoning_reserve_tokens + max_output_tokens
<= model_context_window_tokens
```

当前 estimator 是确定性的启发式估算，不是目标模型 tokenizer；服务端仍是最终裁决者。
RAM-A 不维护模型窗口表，也不会自动猜测模型能力。

## GLM Coding Plan 已验证原则

GLM Coding Plan 可能同时返回 `reasoning_content` 和最终 `content`。短输出预算下也可能只生成
reasoning，导致 content 为空。因此：

- RAM-A 请求显式配置 `reasoning_effort: "none"`；
- Extract/Ground 使用各自的正常输出预算，不应把 smoke test 的 64 token 当生产预算；
- `reasoning_only_retry` 可作为一次有界纠正，不保证服务端一定支持真正的非推理模式；
- Structured Output 模式必须先对实际端点做 smoke test，未经验证时使用 `prompt_only`；
- 成功标准是最终 content 可解析且完整摄入通过，不是仅有 HTTP 200。
