# ram-a-kv 介绍

## 产品概述

ram-a-kv 是一个 **KV 缓存（KV cache）协调守护进程（daemon）**，面向多轮 AI Agent 推理场景，为基于昇腾 NPU 的推理服务提供语义感知的 KV cache 生命周期管理。

在 Agent 工作流中，每一轮推理都依赖此前轮次的中间结果，而现有的 KV cache 系统（如 LMCache-Ascend）是**被动的仓库管理者**——仅按 LRU 策略管理缓存，对 Agent 上下文语义无感知。在多租户压力下，热的 KV cache 块会被驱逐到慢速存储，导致首 Token 延迟（TTFT）从毫秒级飙升到秒级。

ram-a-kv 作为**语义大脑**，将 Agent 上下文映射到 KV cache 块（chunk），预测重用关系，并在下一次推理之前**主动**编排预取（prefetch）/驱逐（evict）/降级（demotion），将 TTFT 压回毫秒级，全面降低多轮推理的延迟与算力开销。

## 支持平台

- **操作系统**：openEuler 26.09 LTS 或更高版本（x86\_64 / aarch64）
- **运行依赖**：LMCache-Ascend 服务（作为 KV cache 后端，提供 HTTP 端点）
- **算力底座**：昇腾 NPU（配套 CANN 驱动）

## 工作模式

ram-a-kv 以守护进程形式运行，通过 HTTP API 接收推理生命周期事件，维护会话级 KV cache map 与全局引用计数，并据此向后端编排 prefetch / evict 操作。

| 层次 | 说明 |
|---|---|
| **事件接入** | 宿主 Agent 在推理轮次的关键节点向 daemon 上报事件 |
| **状态管理** | daemon 维护每个会话的 chunk map 与全局引用计数，持久化到 SQLite |
| **后端编排** | daemon 根据事件与引用计数，向 LMCache-Ascend 后端发送 prefetch / evict / query 请求 |

无论哪种宿主 Agent，事件契约、状态模型与后端编排逻辑都保持一致。

## 功能特性

- **事件驱动**：9 个事件覆盖推理轮次的完整生命周期（`turn_start` / `turn_end` / `snapshot_restore` / `session_map` / `session_close` / `session_suspend` / `session_fork` / `session_fork_end` / `health`）
- **引用计数安全**：跨会话共享的 chunk 通过全局引用计数（refcount）保护，杜绝被某个会话提前驱逐
- **持久化与重启恢复**：SQLite 存储 session map，daemon 重启后自动遍历恢复所有会话状态与引用计数
- **安全认证**：非回环地址监听时强制要求 Bearer 令牌认证，防止未授权访问
