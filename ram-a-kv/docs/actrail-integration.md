<!-- 本文件描述 ram-a-kv 作为 actrail-kv 采集中转入口的对接方式与使用方法。 -->
# ram-a-kv 对接 actrail-kv

actrail-kv 从 `0.2.0` 起支持两种接入方式。RAM-A daemon 通过 HTTP 转发采集请求，由 Actrail 命令行工具完成分析和报告。需要在应用进程内调用这三项能力时，可使用原生动态库。Rust 应用通过 `actrail-kv-sdk` 加载这些库。

## 1. 架构

ram-a-kv 作为推理侧 daemon，位于 Agent 与模型后端之间，可作为 actrail-kv 的采集中转入口：Agent 在 `turn_end` 事件里附带完整 LLM 请求，ram-a-kv 补齐部署维度后转发给 actrail-kv-receiver。

```text
Agent ──turn_end(含 actrail_capture)──▶ ram-a-kv ──forward──▶ actrail-kv-receiver → requests.ndjson
                                          │                        → analyze → report.html
                                          └─ KV 协调主流程不受影响
```

维度补齐分工：Agent 填 `endpoint_key` / `agent_key`，ram-a-kv 填 `model_deployment_key` / `kv_namespace`，receiver 落盘。转发为 fire-and-forget，失败只 warn，不阻塞主流程。

## 2. 使用方法

### 2.1 配置

ram-a-kv 配置文件新增三个字段：

| 字段 | 作用 |
|---|---|
| `actrail_receiver_url` | receiver 地址，留空 = 禁用转发 |
| `actrail_model_deployment_key` | 模型部署标识（actrail-kv 聚类维度） |
| `actrail_kv_namespace` | KV namespace（actrail-kv 聚类维度） |

```toml
actrail_receiver_url = "http://127.0.0.1:8087"
actrail_model_deployment_key = "GLM-5.2"
actrail_kv_namespace = "default"
```

多部署场景下，每个部署运行独立的 ram-a-kv 实例，各自配置不同的 `actrail_model_deployment_key`（如 `glm-5.2-bj` / `glm-5.2-sh`），避免跨部署误聚类。

### 2.2 启动

```bash
# 1. 启动 actrail-kv-receiver
actrail-kv-receiver --listen 127.0.0.1:8087 --output requests.ndjson

# 2. 启动 ram-a-kv
RAM_A_KV_CONFIG=config.toml ram-a-kv

# 3. 正常使用 Agent（SDK daemon_url 指向 ram-a-kv）
```

ram-a-kv 启动时日志输出 `actrail-kv relay enabled` 表示转发已开启。

Actrail 命令默认读取 `~/.actrail/actrail-kv/settings.toml`，并补齐所需的默认设置。可用 `--config <path>` 指定其他 TOML 文件。示例中的 `--output` 选择单文件 NDJSON。省略该参数和配置中的 `receiver.output` 时，receiver 使用 `[storage]`，默认写入设置文件旁的 `captures.sqlite`。

SQLite 和托管 NDJSON 同时按保留期和容量清理采集记录。

| 配置项 | 默认值 | 清理规则 |
|---|---|---|
| `storage.retention.max_age_seconds` | 7 天（`604800` 秒） | 按采集时间清理过期记录。`0` 关闭按时间清理。 |
| `storage.retention.max_size_bytes` | 256 MiB（`268435456` 字节） | 容量不足时清理最旧的数据。容量上限始终生效。 |

### 2.3 生成报告

```bash
actrail-kv-analyze --input requests.ndjson --output analysis.json --top-k 20
actrail-kv-report  --input analysis.json  --output report.html
```

通过 `--config` 启动 receiver 后，analyze 和 report 可指定同一份设置并省略输入、输出参数。analyze 读取配置指定的采集记录，report 默认读取分析产物。设置文件内的相对路径以该文件所在目录为基准。

使用 `--session-key` 筛选会话，使用 `--time-range FROM..UNTIL` 筛选采集时间。`--last-n` 在会话和时间筛选之后选取最新的 N 条记录。

### 2.4 验证

```bash
# 日志出现 "actrail forward ok status=202" = 转发成功
tail -f ram-a-kv.log

# 语料累积
wc -l requests.ndjson
```

## 3. 故障隔离

转发失败不影响 Agent 调模型，也不影响 ram-a-kv 的 KV 协调主流程。receiver 挂了只 warn，ram-a-kv 挂了只 warn，Agent 照常工作。

## 4. 在应用内调用 actrail-kv

从 `0.2.0` 升到 `0.2.1` 时，Rust 应用需要同时更新 `actrail-kv-sdk` 和动态库。C 应用使用新版 `actrail.h`。导出符号的前缀改为 `actrail_kv_`。

创建实例时，宿主提供动态库路径和绝对路径 `base_dir`。`SdkConfig.config_toml` 为 `None` 时读取默认 `~/.actrail/actrail-kv/settings.toml`，缺项会补齐写回。传入 `Some(text)` 时使用内存 TOML。`SdkOverrides` 的显式字段优先，仅对当前实例生效。

文件配置的相对路径以设置文件所在目录为基准。内存配置和显式导出的相对路径以 `base_dir` 为基准。

在 Linux 上，三项能力按以下顺序使用。

1. 用 `libactrail_receiver.so` 提交请求，将采集记录写入存储。
2. 用 `libactrail_analyzer.so` 从同一存储筛选记录，获取内存中的分析结果。
3. 用 `libactrail_reporter.so` 渲染该结果，获取内存中的报告。需要文件时，调用方显式导出 JSON、HTML 或 Markdown。

Rust 调用示例和 C 接口分别见 [sdk_pipeline.rs](../actrail-kv/crates/sdk/rust/examples/sdk_pipeline.rs) 和 [actrail.h](../actrail-kv/crates/sdk/native/contracts/include/actrail.h)。
