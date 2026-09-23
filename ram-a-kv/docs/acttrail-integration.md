<!-- 本文件描述 ram-a-kv 作为 actrail-kv 采集中转入口的对接方式与使用方法。 -->
# ram-a-kv 对接 actrail-kv

## 1. 架构

ram-a-kv 作为推理侧 daemon，位于 Agent 与模型后端之间，可作为 actrail-kv 的采集中转入口：Agent 在 `turn_end` 事件里附带完整 LLM 请求，ram-a-kv 补齐部署维度后转发给 actrail-kv-receiver。

```text
Agent ──turn_end(含 acttrail_capture)──▶ ram-a-kv ──forward──▶ actrail-kv-receiver → requests.ndjson
                                          │                        → analyze → report.html
                                          └─ KV 协调主流程不受影响
```

维度补齐分工：Agent 填 `endpoint_key` / `agent_key`，ram-a-kv 填 `model_deployment_key` / `kv_namespace`，receiver 落盘。转发为 fire-and-forget，失败只 warn，不阻塞主流程。

## 2. 使用方法

### 2.1 配置

ram-a-kv 配置文件新增三个字段：

| 字段 | 作用 |
|---|---|
| `acttrail_receiver_url` | receiver 地址，留空 = 禁用转发 |
| `acttrail_model_deployment_key` | 模型部署标识（actrail-kv 聚类维度） |
| `acttrail_kv_namespace` | KV namespace（actrail-kv 聚类维度） |

```toml
acttrail_receiver_url = "http://127.0.0.1:8087"
acttrail_model_deployment_key = "GLM-5.2"
acttrail_kv_namespace = "default"
```

多部署场景下，每个部署运行独立的 ram-a-kv 实例，各自配置不同的 `acttrail_model_deployment_key`（如 `glm-5.2-bj` / `glm-5.2-sh`），避免跨部署误聚类。

### 2.2 启动

```bash
# 1. 启动 actrail-kv-receiver
actrail-kv-receiver --listen 127.0.0.1:8087 --output requests.ndjson

# 2. 启动 ram-a-kv
RAM_A_KV_CONFIG=config.toml ram-a-kv

# 3. 正常使用 Agent（SDK daemon_url 指向 ram-a-kv）
```

ram-a-kv 启动时日志输出 `acttrail-kv relay enabled` 表示转发已开启。

### 2.3 生成报告

```bash
actrail-kv-analyze --input requests.ndjson --output analysis.json --top-k 20
actrail-kv-report  --input analysis.json  --output report.html
```

### 2.4 验证

```bash
# 日志出现 "acttrail forward ok status=202" = 转发成功
tail -f ram-a-kv.log

# 语料累积
wc -l requests.ndjson
```

## 3. 故障隔离

转发失败不影响 Agent 调模型，也不影响 ram-a-kv 的 KV 协调主流程。receiver 挂了只 warn，ram-a-kv 挂了只 warn，Agent 照常工作。
