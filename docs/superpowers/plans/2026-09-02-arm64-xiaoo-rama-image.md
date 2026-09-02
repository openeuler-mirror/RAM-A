# ARM64 xiaoO + RAM-A 即用镜像实施计划

> **供自动化执行者使用：** 必须使用 `superpowers:executing-plans` 按任务执行本计划；步骤使用复选框跟踪。

**目标：** 构建一个基于 openEuler 24.03 LTS SP3、安装最新 xiaoO aarch64 RPM、编译 AtomGit PR 18 最新提交并内置运行配置与凭据的 `linux/arm64` 即用镜像，同时验证 `ram-a-mem` 记忆摄入、幂等和检索链路。

**架构：** 使用官方多架构 `openeuler/openeuler:24.03-lts-sp3` 进行 ARM64 多阶段构建。构建阶段从 AtomGit 的 `refs/merge-requests/18/head` 拉取源码并编译 `ram-a-mem`；运行阶段从 EulerMaker Packages 索引选择最新主包，安装配置、凭据与运维脚本。主机侧 PowerShell 脚本负责构建、启动和验收，容器内 Shell 脚本负责服务编排与 MCP 调用。

**技术栈：** Docker BuildKit/buildx、openEuler 24.03 LTS SP3 ARM64、RPM/DNF、Rust/Cargo、Bash、PowerShell、Python 标准库、HTTP MCP、SQLite、GLM Coding Plan。

---

## 文件结构

- `deploy/arm64/Dockerfile`：ARM64 多阶段编译、RPM 安装、配置与凭据固化。
- `deploy/arm64/.dockerignore`：排除 Git、历史产物和密钥文件。
- `deploy/arm64/config/*`：RAM-A、xiaoO 和 MCP 配置。
- `deploy/arm64/scripts/*`：启动、健康检查、MCP、摄入验收、更新编译和镜像信息脚本。
- `deploy/arm64/*.ps1`：Windows 主机上的构建、启动和验收入口。
- `deploy/arm64/tests/test_assets.py`：部署资产静态契约测试。

### 任务 1：建立部署资产契约测试

**文件：**
- 新建：`deploy/arm64/tests/test_assets.py`

- [ ] **步骤 1：编写失败测试**

```python
from pathlib import Path
import json
import unittest

ROOT = Path(__file__).resolve().parents[1]


class Arm64ImageAssetsTest(unittest.TestCase):
    def test_required_assets_exist(self):
        required = {
            "Dockerfile", ".dockerignore", "config/ram-a-mem.json",
            "config/xiaoo.toml", "config/xiaoo.mcp.json",
            "scripts/start-ram-a.sh", "scripts/start-xiaoo.sh",
            "scripts/start-all.sh", "scripts/healthcheck.sh",
            "scripts/mcp.sh", "scripts/verify-ingest.sh",
            "scripts/rebuild-ram-a.sh", "scripts/image-info.sh",
            "build-image.ps1", "run-image.ps1", "verify-image.ps1",
        }
        missing = sorted(str(p) for p in required if not (ROOT / p).is_file())
        self.assertEqual([], missing)

    def test_dockerfile_builds_pr18_for_arm64(self):
        text = (ROOT / "Dockerfile").read_text(encoding="utf-8")
        self.assertIn("openeuler/openeuler:24.03-lts-sp3", text)
        self.assertIn("refs/merge-requests/18/head", text)
        self.assertIn("cargo build --release --locked -p memory-mcp", text)
        self.assertIn("ARG GLM_CODING_TOKEN", text)
        self.assertIn("ARG OPENROUTER_API_KEY", text)

    def test_ram_a_config_uses_hash(self):
        config = json.loads((ROOT / "config/ram-a-mem.json").read_text())
        self.assertEqual("hash", config["providers"]["embedding_provider"])
        self.assertEqual(1024, config["providers"]["embedding_dimensions"])
        self.assertEqual("GLM-5.2", config["providers"]["extractor_model"])
        self.assertFalse(config["features"]["graph_memory"]["enabled"])
        self.assertFalse(config["retrieval"]["rerank"]["enabled"])

    def test_repository_assets_do_not_contain_real_tokens(self):
        forbidden = ("f12f159980484722", "sk-or-v1-87ad6b14")
        for path in ROOT.rglob("*"):
            if path.is_file() and path.suffix not in {".pyc", ".rpm"}:
                text = path.read_text(encoding="utf-8", errors="ignore")
                for prefix in forbidden:
                    self.assertNotIn(prefix, text, str(path))


if __name__ == "__main__":
    unittest.main()
```

- [ ] **步骤 2：确认测试先失败**

运行：`python -m unittest discover -s deploy/arm64/tests -v`

预期：因 Dockerfile、配置和脚本尚不存在而失败。

- [ ] **步骤 3：提交测试**

运行：`git add deploy/arm64/tests/test_assets.py && git commit -m "test: define arm64 image asset contract"`

### 任务 2：创建 RAM-A 与 xiaoO 配置

**文件：**
- 新建：`deploy/arm64/config/ram-a-mem.json`
- 新建：`deploy/arm64/config/xiaoo.toml`
- 新建：`deploy/arm64/config/xiaoo.mcp.json`
- 新建：`deploy/arm64/.dockerignore`

- [ ] **步骤 1：创建 RAM-A 配置**

使用 PR 18 指南中的个人记忆最小配置：监听 `0.0.0.0:18081`，存储 `/var/lib/ram-a/ram-a-memory.sqlite`，Chat Provider 为 `GLM_CODING_TOKEN` 和 `https://open.bigmodel.cn/api/coding/paas/v4`，模型 `GLM-5.2`；embedding 为 `hash/1024`；graph、case library、rerank 均关闭；认证令牌环境变量为 `RAM_A_XIAOO_TOKEN`。

- [ ] **步骤 2：创建 xiaoO 配置**

`xiaoo.toml` 设置 `provider = "anthropic"`、`api_base = "https://open.bigmodel.cn/api/anthropic"`、`model = "GLM-5.2"`、`api_key_env = "ANTHROPIC_AUTH_TOKEN"`，并开启 `memory_automation`。`xiaoo.mcp.json` 使用 streamable HTTP 连接 `http://127.0.0.1:18081/mcp`。

- [ ] **步骤 3：创建 `.dockerignore`**

排除 `.git`、`target`、`outputs`、`.pytest_cache`、`*.rpm`、`.env*` 和验收结果，确保真实密钥不进入构建上下文。

- [ ] **步骤 4：验证并提交**

运行：`python -m unittest discover -s deploy/arm64/tests -v`

预期：配置测试通过，文件完整性测试仍失败。

提交：`git add deploy/arm64/config deploy/arm64/.dockerignore && git commit -m "feat: add arm64 RAM-A and xiaoO configuration"`

### 任务 3：创建容器内运行和验收脚本

**文件：**
- 新建：`deploy/arm64/scripts/start-ram-a.sh`
- 新建：`deploy/arm64/scripts/start-xiaoo.sh`
- 新建：`deploy/arm64/scripts/start-all.sh`
- 新建：`deploy/arm64/scripts/healthcheck.sh`
- 新建：`deploy/arm64/scripts/mcp.sh`
- 新建：`deploy/arm64/scripts/verify-ingest.sh`
- 新建：`deploy/arm64/scripts/rebuild-ram-a.sh`
- 新建：`deploy/arm64/scripts/image-info.sh`

- [ ] **步骤 1：创建健康检查**

```bash
#!/usr/bin/env bash
set -euo pipefail
curl --fail --silent --show-error http://127.0.0.1:18081/healthy >/dev/null
curl --fail --silent --show-error http://127.0.0.1:18081/ready >/dev/null
```

- [ ] **步骤 2：创建 RAM-A 启动脚本**

脚本检查 `GLM_CODING_TOKEN` 与 `RAM_A_XIAOO_TOKEN`，创建数据/日志/PID 目录，后台运行 `ram-a-mem --config /etc/ram-a/ram-a-mem.json`，写 PID，并在 120 秒内等待 `/ready`；进程提前退出时打印 RAM-A 日志并失败。

- [ ] **步骤 3：创建 MCP 客户端**

复用 `ram-a-mem/docs/guides/ram-a-mem-rpm-agent-self-test.zh-CN.md` 第 6 节已经验收的 initialize、initialized、tools/list、tools/call 实现，状态目录固定为 `/var/lib/ram-a/selftest`，协议版本保持 `2025-11-25`。

- [ ] **步骤 4：创建摄入验收脚本**

脚本依次验证 GLM `/chat/completions`、MCP 初始化、工具列表、唯一 marker 首次摄入、相同参数幂等命中、`memory_search` 命中 marker、规范阶段日志。要求首次 `accepted_count >= 1`、至少一个 memory ID、二次 `idempotency_hit == true` 且 IDs 相同。结果写入 `/var/lib/ram-a/selftest/results`，成功输出 `RAM_A_INGEST_OK marker=<值>`，并确认结果/日志不含两个真实 token。

- [ ] **步骤 5：创建 xiaoO 和默认入口**

`start-xiaoo.sh` 执行：

```bash
exec xiaoo --cli --mcp-config /etc/xiaoo/mcp.json \
  run --config /etc/xiaoo/config.toml --debug "$@"
```

`start-all.sh` 先执行 `start-ram-a.sh`；首参数为 `verify` 时运行验收脚本，否则进入 xiaoO。

- [ ] **步骤 6：创建更新与信息脚本**

`rebuild-ram-a.sh` 在 `/opt/RAM-A` 执行以下确定命令：

```bash
git fetch upstream refs/merge-requests/18/head
git checkout -B pr18 FETCH_HEAD
cargo test --manifest-path ram-a-mem/Cargo.toml -p memory-pipeline --test offline_pipeline
cargo build --manifest-path ram-a-mem/Cargo.toml --release --locked -p memory-mcp
install -m 0755 ram-a-mem/target/release/ram-a-mem /usr/local/bin/ram-a-mem
git rev-parse HEAD >/opt/metadata/ram-a-commit
```

`image-info.sh` 只输出架构、`rpm -q xiaoO`、RPM SHA-256、RAM-A commit 和帮助信息，不读取凭据值。

- [ ] **步骤 7：验证并提交**

运行静态测试；提交：`git add deploy/arm64/scripts && git commit -m "feat: add image runtime and ingestion verification scripts"`。

### 任务 4：创建 ARM64 多阶段 Dockerfile

**文件：**
- 新建：`deploy/arm64/Dockerfile`

- [ ] **步骤 1：创建编译阶段**

以 `FROM --platform=linux/arm64 openeuler/openeuler:24.03-lts-sp3 AS builder` 开始，安装 Rust、Cargo、Git、编译器和 OpenSSL/SQLite 依赖。在 `/opt/RAM-A` 初始化 Git，执行：

```bash
git remote add upstream https://atomgit.com/openeuler/RAM-A.git
git fetch --depth 1 upstream refs/merge-requests/18/head
git checkout -B pr18 FETCH_HEAD
cargo test --manifest-path ram-a-mem/Cargo.toml -p memory-pipeline --test offline_pipeline
cargo build --manifest-path ram-a-mem/Cargo.toml --release --locked -p memory-mcp
```

记录 `git rev-parse HEAD`，并使用 BuildKit cache mount 缓存 Cargo registry 和 target。

- [ ] **步骤 2：创建运行阶段与最新版 RPM 解析**

同一 ARM64 基础镜像中安装运行/更新依赖。从用户提供的 `XIAOO_PACKAGES_URL` 获取 HTML，用以下过滤器选择最新主包：

```bash
rpm_name="$(curl -fsSL "$XIAOO_PACKAGES_URL" \
  | sed -n 's/.*href="\(xiaoO-[0-9][^"]*\.aarch64\.rpm\)".*/\1/p' \
  | sort -V | tail -n 1)"
test -n "$rpm_name"
curl -fL --retry 3 "${XIAOO_PACKAGES_URL}${rpm_name}" -o /tmp/xiaoo.rpm
test "$(rpm -qp --qf '%{ARCH}' /tmp/xiaoo.rpm)" = aarch64
dnf install -y /tmp/xiaoo.rpm
```

保存 RPM、NEVRA、SHA-256 和 RAM-A commit 到 `/opt/metadata`。

- [ ] **步骤 3：按用户要求内置凭据**

声明 `ARG GLM_CODING_TOKEN`、`ARG OPENROUTER_API_KEY`，验证非空后将它们固化为最终镜像 `ENV`；同时设置 `ANTHROPIC_AUTH_TOKEN`、`ANTHROPIC_BASE_URL`、`RAM_A_XIAOO_TOKEN`、`RAM_A_MEM_CONFIG`。Dockerfile 与 Git 文件中不出现真实值。

- [ ] **步骤 4：安装配置、脚本和默认入口**

复制编译结果、`/opt/RAM-A` Git 工作区、配置和脚本；创建持久化目录；声明两个 VOLUME、18081 端口、`healthcheck.sh` 和 `start-all.sh` ENTRYPOINT。

- [ ] **步骤 5：运行静态测试并提交**

运行：`python -m unittest discover -s deploy/arm64/tests -v`

预期：全部通过。

提交：`git add deploy/arm64/Dockerfile && git commit -m "feat: build arm64 xiaoO and RAM-A image"`。

### 任务 5：创建主机侧构建、启动和验收脚本

**文件：**
- 新建：`deploy/arm64/build-image.ps1`
- 新建：`deploy/arm64/run-image.ps1`
- 新建：`deploy/arm64/verify-image.ps1`

- [ ] **步骤 1：创建构建脚本**

脚本从参数或当前进程环境读取两个密钥，缺失即失败且不打印值，然后执行：

```powershell
docker buildx build --platform linux/arm64 --load `
  --build-arg "GLM_CODING_TOKEN=$GlmToken" `
  --build-arg "OPENROUTER_API_KEY=$OpenRouterKey" `
  --tag $ImageName $PSScriptRoot
```

默认镜像名为 `xiaoo-rama:arm64-pr18`。

- [ ] **步骤 2：创建启动脚本**

创建 `ram-a-data`、`xiaoo-data` 命名卷，运行 ARM64 镜像并映射 18081；默认进入 xiaoO，传入 `verify` 时直接执行验收。

- [ ] **步骤 3：创建验收脚本**

先用 `docker image inspect` 检查 Architecture 为 arm64，再分别运行 `image-info.sh` 和 `verify-ingest.sh`，检查退出码和 `RAM_A_INGEST_OK`。

- [ ] **步骤 4：验证并提交**

运行静态测试；提交：`git add deploy/arm64/*.ps1 && git commit -m "feat: add arm64 image host workflows"`。

### 任务 6：构建与端到端验收

**文件：**
- 可能修改：`deploy/arm64/Dockerfile`
- 可能修改：`deploy/arm64/scripts/*.sh`

- [ ] **步骤 1：在当前 PowerShell 进程设置用户提供的完整凭据**

不写入仓库文件，不输出到终端；构建完成后镜像环境中应能检查到变量名和值非空。

- [ ] **步骤 2：执行 ARM64 构建**

运行：`./deploy/arm64/build-image.ps1 -ImageName xiaoo-rama:arm64-pr18`

预期：拉取 PR 18、通过离线 pipeline 测试、编译 release、安装 aarch64 xiaoO RPM并加载镜像。

- [ ] **步骤 3：按系统化调试修正真实构建问题**

若遇 Rust 版本、RPM 依赖、QEMU 或 CLI 差异，先记录失败命令和根因，增加最小测试，再只修改相关文件；不得绕过 PR 18、RPM 架构、凭据非空和摄入验收检查。

- [ ] **步骤 4：验证镜像元数据与摄入管线**

运行：

```powershell
docker image inspect xiaoo-rama:arm64-pr18 --format '{{.Architecture}}'
./deploy/arm64/verify-image.ps1 -ImageName xiaoo-rama:arm64-pr18
```

预期：架构 arm64、xiaoO 为仓库最新 NEVRA、RAM-A commit 为构建时 PR 18 head；模型、MCP、首次摄入、幂等、检索和阶段日志全部通过。

- [ ] **步骤 5：运行 PR 18 日志相关测试**

在最终镜像 `/opt/RAM-A` 内运行 `memory-pipeline --test pipeline_logging` 和 `memory-mcp --test logging`，要求全部通过。

- [ ] **步骤 6：验收成功后删除旧镜像**

先确认旧 `xiaoo:latest` ID 仍为 `070ff3dccc0a...` 且新镜像通过全部检查，再运行 `docker image rm xiaoo:latest`。若被运行中容器引用则保留并报告，禁止强制删除或删除其他标签。

- [ ] **步骤 7：最终状态检查**

运行 `git status --short --branch` 和 `docker image ls`。保留用户原有的 `outputs/`、`target/`，报告新镜像 ID/大小、xiaoO 版本、PR 18 commit 和摄入验收结果。

---

## 自检结论

- 已覆盖 ARM64、PR 18 最新 head、最新 xiaoO RPM、hash embedding、GLM/OpenRouter 内置、即用脚本、摄入/幂等/检索/日志验证和成功后删除旧镜像。
- 真实密钥只通过构建进程传入并固化到最终镜像，不提交到 Git，不写入计划或日志。
- 构建、RPM、运行和验证均固定 `linux/arm64`。
- 任一强制验收失败时保留旧镜像，不宣称完成。

