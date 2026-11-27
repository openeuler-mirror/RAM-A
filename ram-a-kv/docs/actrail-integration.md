<!-- 本文件描述 ram-a-kv 作为 actrail-kv 采集中转入口的对接方式与使用方法。 -->
# ram-a-kv 对接 actrail-kv

ram-a-kv 从 `0.2.1` 起通过 `actrail-kv-sdk` 以进程内动态库的方式调用 actrail-kv 的 receiver 能力，无需独立运行 `actrail-kv-receiver` 进程。分析与报告仍由 Actrail 命令行工具离线完成。

## 1. 架构

ram-a-kv 作为推理侧 daemon，位于 Agent 与模型后端之间，作为 actrail-kv 的采集中转入口：Agent 在 `turn_end` 事件里附带完整 LLM 请求，ram-a-kv 补齐部署维度后，通过 SDK in-process 调用 `libactrail_receiver.so` 将采集记录写入存储。

```text
Agent ──turn_end(含 actrail_capture)──▶ ram-a-kv ──SDK submit──▶ libactrail_receiver.so → captures.sqlite / captures/
                                          │
                                          └─ KV 协调主流程不受影响
```

维度补齐分工：Agent 填 `endpoint_key` / `agent_key`，ram-a-kv 填 `model_deployment_key` / `kv_namespace`，receiver 落盘。转发为 fire-and-forget，失败只 warn，不阻塞主流程。

## 2. 使用方法

### 2.1 配置

ram-a-kv 配置文件新增三个字段：

| 字段 | 作用 |
|---|---|
| `actrail_sdk_lib` | `libactrail_receiver.so` 绝对路径，留空 = 禁用 actrail 集成 |
| `actrail_model_deployment_key` | 模型部署标识（actrail-kv 聚类维度） |
| `actrail_kv_namespace` | KV namespace（actrail-kv 聚类维度） |

```toml
actrail_sdk_lib = "/usr/lib/actrail-kv/libactrail_receiver.so"
actrail_model_deployment_key = "GLM-5.2"
actrail_kv_namespace = "default"
```

多部署场景下，每个部署运行独立的 ram-a-kv 实例，各自配置不同的 `actrail_model_deployment_key`（如 `glm-5.2-bj` / `glm-5.2-sh`），避免跨部署误聚类。

### 2.2 启动

```bash
# 1. 确保 libactrail_receiver.so 已部署到配置指定路径
# 2. 启动 ram-a-kv
RAM_A_KV_CONFIG=config.toml ram-a-kv

# 3. 正常使用 Agent（SDK daemon_url 指向 ram-a-kv）
```

ram-a-kv 启动时日志输出 `actrail-kv sdk enabled` 表示 SDK 已加载；`actrail-kv sdk disabled` 表示未配置 `actrail_sdk_lib`；加载失败会拒绝启动（fail-fast）。

actrail-kv 的 receiver 通过 SDK 读取默认设置文件 `~/.actrail/actrail-kv/settings.toml`（缺项自动补齐写回）。省略 `receiver.output` 时，receiver 使用 `[storage]`，默认写入设置文件旁的 `captures.sqlite`。

SQLite 和托管 NDJSON 同时按保留期和容量清理采集记录。

| 配置项 | 默认值 | 清理规则 |
|---|---|---|
| `storage.retention.max_age_seconds` | 7 天（`604800` 秒） | 按采集时间清理过期记录。`0` 关闭按时间清理。 |
| `storage.retention.max_size_bytes` | 256 MiB（`268435456` 字节） | 容量不足时清理最旧的数据。容量上限始终生效。 |

### 2.3 生成报告

```bash
actrail-kv-analyze --config ~/.actrail/actrail-kv/settings.toml --top-k 20
actrail-kv-report  --config ~/.actrail/actrail-kv/settings.toml --output report.html
```

analyze 读取配置指定的采集记录，report 默认读取分析产物。设置文件内的相对路径以该文件所在目录为基准。

使用 `--session-key` 筛选会话，使用 `--time-range FROM..UNTIL` 筛选采集时间。`--last-n` 在会话和时间筛选之后选取最新的 N 条记录。

### 2.4 验证

```bash
# 日志出现 "actrail sdk submit ok" = 采集成功
tail -f ram-a-kv.log

# 语料累积
ls -la ~/.actrail/actrail-kv/captures.sqlite
```

## 3. 故障隔离

SDK submit 失败不影响 Agent 调模型，也不影响 ram-a-kv 的 KV 协调主流程，只 warn。但 daemon 启动时若 `.so` 文件加载失败或 ABI 不匹配，daemon 拒绝启动（fail-fast），避免运行期静默丢采集。

## 4. SDK 调用细节

ram-a-kv 通过 `actrail-kv-sdk` 的 `ReceiverFactory::create(SdkConfig)` 加载 `libactrail_receiver.so`，创建一个 in-process 的 `Receiver` 实例：

- `library_path`：配置文件的 `actrail_sdk_lib`
- `base_dir`：ram-a-kv 配置文件所在目录（用于显式导出的相对路径基准）
- `config_toml`：`None`（读默认 `~/.actrail/actrail-kv/settings.toml`）
- `overrides`：默认（不覆盖设置文件）

`turn_end` 时，ram-a-kv 把 `actrail_capture` 字段组装成 `CaptureInput`（含 `payload` / `source` / `session_key` / `comparison`），通过 `tokio::task::spawn_blocking` 调用 `Receiver::submit()`（同步阻塞写存储），fire-and-forget，不阻塞 `turn_end` 响应。

`Receiver` 实例用 `Arc<Mutex<Receiver>>` 共享，临界区仅覆盖 `submit()` 调用，锁竞争极小。

Rust SDK 完整调用示例见 [sdk_pipeline.rs](../actrail-kv/crates/sdk/rust/examples/sdk_pipeline.rs)，C 接口见 [actrail.h](../actrail-kv/crates/sdk/native/contracts/include/actrail.h)。
