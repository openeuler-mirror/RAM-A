# ram-a-kv 安装指南

## 环境要求

- **操作系统**：openEuler 26.09 LTS 或更高版本（x86\_64 / aarch64）
- **内存**：建议至少 8GB RAM
- **存储**：建议至少 10GB 可用磁盘空间（含 SQLite 持久化）
- **KV cache 后端服务**：
    - 需部署 LMCache-Ascend 服务（HTTP 端点 `/memory/prefetch`、`/memory/evict`、`/memory/query`），配套昇腾 NPU 与 CANN 驱动
    - 具体安装与配置见 LMCache-Ascend 官方文档

## 安装

安装 ram-a-kv：

```bash
sudo yum install ram-a-kv
```

启动 daemon（前台运行）：

```bash
RAM_A_KV_CONFIG=/etc/ram-a-kv/config.toml ram-a-kv
```

另开终端，发送健康检查事件验证 daemon 是否正常响应：

```bash
curl -s -X POST http://127.0.0.1:6998/event \
  -H 'Content-Type: application/json' \
  -d '{"type":"health"}' | jq
```

期望返回：

```json
{
  "ok": true,
  "type": "health",
  "data": {
    "status": "running",
    "sessions_count": 0
  }
}
```

## 配置参数

daemon 通过环境变量 `RAM_A_KV_CONFIG` 指定配置文件路径；未设置时默认读取 `/etc/ram-a-kv/config.toml`。

配置项（TOML 顶层字段）：

| 字段 | 默认值 | 说明 |
|---|---|---|
| `listen_addr` | `127.0.0.1:6998` | HTTP 监听地址 |
| `lmcache_url` | `http://localhost:6999` | LMCache-Ascend 后端地址 |
| `debug_enabled` | `false` | 是否写 debug 文件 |
| `debug_dir` | `kvcache_debug` | debug 文件输出目录 |
| `auth_token` | （空） | Bearer 令牌；非回环地址监听时**必填** |

示例：

```toml
listen_addr = "127.0.0.1:6998"
lmcache_url = "http://localhost:6999"
debug_enabled = false
debug_dir = "kvcache_debug"
```
