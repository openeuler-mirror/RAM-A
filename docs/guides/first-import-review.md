# RAM-A First Import Review Notes

本文用于同事 review 首批从 `memory-euler` 搬迁到 `RAM-A` 的范围、已完成事项和仍需校对的决策。

阅读时可以按三层看：

1. **基础搬迁范围**：已经完成，原则上只是确认文件、目录和验证结果是否完整。
2. **需要校对的裁决**：已经按当前判断落地，但建议同事重点看决定是否合理。
3. **后续 TODO**：不阻塞首批导入，但后面需要继续做。

## 1. 基础搬迁范围

这些内容属于首批导入的基础工作，当前不再作为争议点处理。

### 1.1 已导入目录

```text
RAM-A/
  Cargo.toml
  Cargo.lock
  README.md
  .gitattributes
  .gitignore

  crates/
    memory-core/
    memory-bench/

  docs/
    README.md
    benchmarks/
      prepared-schema-v1.md
    design/
      memory-pipeline-roadmap.md
      sqlite-hybrid-search.md
    guides/
      locomo-evaluation.md
      first-import-review.md

  data/
    locomo/
      README.md
      locomo10.json

  evaluation/
    README.md
    README.zh-CN.md
    requirements.txt
    run_locomo_eval.sh
    common/
    clients/
      mem0_local.py
    fixtures/
      sample.json
      personalmem_sample.json
      locomo_sample.json
    datasets/
      README.md
    baselines/
      README.md
      index.example.jsonl
    personalmem/
    longmemeval/
    locomo/
      backends/
        mem0/
    scripts/
      benchmark_dashboard.py
      run_personalmem_ram_a_v1.sh
      run_personalmem_mem0_local_v1.sh
```

### 1.2 已完成基础事项

- Rust workspace、核心 crate、benchmark CLI、评测脚本、文档和小型 fixtures 已搬入 `RAM-A`。
- 顶层 README 已改为正式 `RAM-A` 表述，不再保留 `xiaoO` 抽离叙事。
- 文档已按 `docs/benchmarks/`、`docs/design/`、`docs/guides/` 分层。
- `evaluation/fixtures/` 只保留轻量 smoke 数据：通用 sample、PersonaMem sample、合成 LoCoMo sample。
- 完整 LoCoMo benchmark 文件保留在 `data/locomo/locomo10.json`，不再放在 fixtures。
- `evaluation/baselines/` 已加入 Git-friendly 的结果索引模板，用于后续记录跑分摘要。
- `memory-euler` 原仓保持为对照仓，当前没有 tracked 改动。

### 1.3 已完成评测适配

- LongMemEval、PersonaMem、LoCoMo 的 RAM-A backend metadata 已统一写入 `RAM-A`。
- LoCoMo 源码已从 `evaluation/scripts/locomo/` 拆到 `evaluation/locomo/`。
- LoCoMo judge 调用已统一到 RAM-A OpenAI-compatible client，不再在导入期依赖 `mem0ai.extract_json`。
- PersonaMem v1 shell wrapper 已保留为一键式 full-run 入口，并清理旧 `memory_euler` 命名。
- LoCoMo mem0 对比后端已移到 `evaluation/locomo/backends/mem0/`。
- 统一展示 dashboard 当前按 run 目录读取 LoCoMo artifact，不依赖 LoCoMo 源码目录。

### 1.4 提交前敏感信息扫描

本轮提交前已做基础可提交性扫描，当前结论如下：

- 未发现 `.env*` 文件、私钥块、真实 OpenAI/GitHub/AWS/Slack 等密钥形状。
- 未发现公司内部、保密、机密、客户数据、企微、飞书等内部信息字样。
- 未发现设计文档中有“借鉴、复刻、照搬、竞品、市面上某记忆软件”等风险表达。
- `mem0`、`memos`、`MemTensor` 的出现属于评测对照、可选后端或公开 baseline 来源归因，不是设计借鉴表述。
- LoCoMo 完整数据中存在上游图片 URL 的 `token=` query 参数，以及一处图片文件名片段命中 `sk-` 形状；均来自 upstream benchmark 数据，不是 RAM-A 仓库密钥。
- 已清理 guide 验证记录中的本机绝对路径，改为仓库相对命令和 `<repo>` 占位。

### 1.5 未进入首批导入

这些内容可本地保留用于对比，但不建议进入正式仓首批提交：

- `AI_CONTEXT.md`
- `ARCHITECTURE.md`
- `EXTRACTION.md`
- `docs/superpowers/`
- `docs/personalmem_32k_v1_ctx2k_html_artifacts.md`
- `data/personalmem/prepared/personalmem_32k_v1.json`
- `evaluation/results/locomo_results.tar.gz`
- `target/`
- `outputs/`
- Python venv、pytest cache、`__pycache__`

## 2. 需要同事校对的裁决

下面是本轮整理中已经做出的决定。代码和文档已按这些决定调整；review 时重点看这些决定本身是否认可。

| ID | 裁决 | 当前处理 | 建议同事重点看 |
| --- | --- | --- | --- |
| D1 | 完整 LoCoMo `locomo10.json` 放在 `data/locomo/`，不放在 `evaluation/fixtures/`。 | `evaluation/fixtures/` 只保留合成 `locomo_sample.json`；完整文件保留在 `data/locomo/locomo10.json`。 | 是否接受首批导入直接保留完整 LoCoMo 文件。 |
| D2 | LoCoMo 评测代码从 `evaluation/scripts/locomo/` 拆到 `evaluation/locomo/`。 | `run_locomo_eval.sh`、README、测试和 dashboard 路径已同步。 | 是否认可 `evaluation/scripts/` 只保留跨数据集脚本和 shell wrapper。 |
| D3 | LoCoMo mem0 对比后端放在 `evaluation/locomo/backends/mem0/`。 | 不放入全局 `evaluation/backends/`，也不并入 `evaluation/clients/`；通用 mem0 SDK helper 仍在 `evaluation/clients/mem0_local.py`。 | 是否认可它是 LoCoMo-only baseline backend，而不是通用 client。 |
| D4 | `memory-euler` 不再作为 backend key 保留兼容。 | 正式 backend key 为 `RAM-A`；LoCoMo 输出目录使用 `ram-a/`。 | 是否接受不做旧 key 兼容层。 |
| D5 | PersonaMem v1 shell 属于首批导入。 | `run_personalmem_ram_a_v1.sh` 和 `run_personalmem_mem0_local_v1.sh` 作为 full-run convenience wrapper 保留。 | 是否接受 shell wrapper 先保留，后续随 adapter 小步维护。 |
| D6 | `sqlite-hybrid-search.md` 保留为历史设计，不压缩为 ADR。 | 后续 chunk、semantic extraction、timeline-aware reasoning 另放 `docs/design/memory-pipeline-roadmap.md`。 | 是否认可历史实现文档和未来路线图分开维护。 |
| D7 | `evaluation/baselines/index.example.jsonl` 先作为轻量 baseline index 模板。 | 当前不升级为严格 schema；等 dashboard/CI 消费 `index.jsonl` 后再补 `schema_version`、`created_at`、`pipeline_version`、`run_status`、`notes`。 | 是否认可首批导入不复杂化 baseline index。 |
| D8 | `data/` 下可提交 benchmark 输入文件必须带 provenance/license/checksum。 | `data/locomo/README.md` 已写明上游路径、CC BY-NC 4.0、非商业限制、文件大小和 SHA256。 | 是否接受 LoCoMo 的 CC BY-NC 4.0 非商业限制随仓记录方式。 |

## 3. 关键文件说明

| 路径 | 角色 | 当前状态 |
| --- | --- | --- |
| `crates/memory-core/` | 长期记忆核心库，包含 API、record、embedding、store、SQLite hybrid retrieval。 | 已导入；后续可继续 review 核心接口和 SQLite/BM25 拆分边界。 |
| `crates/memory-bench/` | Rust CLI，用于 benchmark add/search。 | 已导入；当前默认走 SQLite/hybrid。 |
| `evaluation/common/` | Python 评测公共能力：metrics、report、runner、backend 抽象。 | 已导入；继续承担跨数据集公共逻辑。 |
| `evaluation/common/backends/` | 评测后端抽象和 RAM-A 后端适配。 | backend key 已统一为 `RAM-A`。 |
| `evaluation/clients/mem0_local.py` | 可复用 mem0 local SDK helper。 | PersonaMem mem0 local 对照使用；依赖 `mem0ai`，可选。 |
| `evaluation/locomo/` | LoCoMo pipeline、报告、retrieval、answer、judge、metrics。 | 已从 scripts 私有目录拆出；评分标准和输出 schema 不变。 |
| `evaluation/locomo/backends/mem0/` | LoCoMo 专属 mem0 对比后端。 | 可选依赖 `mem0ai`；不影响 RAM-A judge 路径。 |
| `evaluation/personalmem/` | PersonaMem adapter、报告和 mem0 local 对比入口。 | wrapper 输出已对齐 `outputs/personalmem/<run>/` artifact 布局。 |
| `evaluation/longmemeval/` | LongMemEval adapter、预处理、QA/retrieval 评测和报告。 | 数据默认约定在 `data/longmemeval/`。 |
| `evaluation/baselines/` | 跑分摘要索引规范和示例 JSONL。 | 首批导入保持轻量模板。 |
| `data/locomo/` | LoCoMo 完整 benchmark 文件和 license/checksum 说明。 | `locomo10.json` SHA256 已记录。 |
| `docs/design/` | 已落地设计说明和后续 roadmap。 | SQLite/hybrid 历史设计与 memory pipeline roadmap 分开。 |

## 4. 验证记录

已在 RAM-A 仓根目录执行过以下验证；命令用仓库相对路径表示：

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python -m pytest evaluation\common evaluation\longmemeval
python -m pytest evaluation\common evaluation\longmemeval evaluation\personalmem evaluation\locomo evaluation\scripts\benchmark_dashboard_test.py -q
python -m pytest evaluation\locomo -q
python -m pytest evaluation -q
python -m py_compile evaluation\locomo\locomo_experiments.py evaluation\locomo\backends\mem0\add.py evaluation\locomo\backends\mem0\search.py
docker run --rm -v "<repo>:/workspace" -w /workspace xiaoo:latest sh -n evaluation/run_locomo_eval.sh
docker run --rm -v "<repo>:/workspace" -w /workspace xiaoo:latest bash -n evaluation/run_locomo_eval.sh
docker run --rm -v "<repo>:/workspace" -w /workspace xiaoo:latest bash -n evaluation/scripts/run_personalmem_ram_a_v1.sh
docker run --rm -v "<repo>:/workspace" -w /workspace xiaoo:latest bash -n evaluation/scripts/run_personalmem_mem0_local_v1.sh
Get-FileHash data\locomo\locomo10.json -Algorithm SHA256
cargo run --quiet -p memory-bench -- --store outputs\locomo_sample_smoke\store.sqlite --embedding hash add --dataset evaluation\fixtures\locomo_sample.json --text-fields text
cargo run --quiet -p memory-bench -- --store outputs\locomo_sample_smoke\store.sqlite --embedding hash search --dataset evaluation\fixtures\locomo_sample.json --query-fields question --top-k 2 --output outputs\locomo_sample_smoke\search_results.json
python evaluation\locomo\locomo_retrieval.py --dataset evaluation\fixtures\locomo_sample.json --input outputs\locomo_sample_smoke\search_results.json --output-json outputs\locomo_sample_smoke\retrieval_metrics.json --html-report outputs\locomo_sample_smoke\retrieval_report.html
```

最近结果摘要：

- Rust tests: 22 passed
- Python expanded smoke after LoCoMo path split + dashboard smoke: 36 passed
- LoCoMo judge interface tests: 6 passed
- Full Python evaluation tests after LoCoMo judge interface cleanup: 40 passed
- Full Python evaluation tests after PersonaMem shell cleanup: 43 passed
- Full Python evaluation tests after LoCoMo mem0 backend relocation: 44 passed
- LoCoMo shell syntax check in `xiaoo:latest`: `sh -n` and `bash -n` passed
- PersonaMem v1 shell syntax check in `xiaoo:latest`: `bash -n` passed for RAM-A and mem0 local wrappers
- LoCoMo data layout smoke: `locomo_sample.json` passed `memory-bench` hash add/search and `locomo_retrieval.py`
- LoCoMo full data integrity: `data/locomo/locomo10.json` SHA256 is `79FA87E90F04081343B8C8DEBECB80A9A6842B76A7AA537DC9FDF651EA698FF4`
- Active backend-key scan: no active `memory-euler` backend key usage remains outside historical migration notes and guard test assertions

合并前仍建议按正式 CI 口径重跑完整验证。

## 5. 后续 TODO

短期：

- 将现有 `run_meta.json` 字段沉淀成轻量 schema 文档或校验测试，避免 PersonaMem、LoCoMo、LongMemEval 的运行元数据继续漂移。
- 给 `evaluation/fixtures/` 增加 fixture 来源说明或 checksum，和 `data/` 下完整 benchmark 文件的 provenance 规则保持一致。
- 合并前按正式仓 CI 口径重跑 `cargo fmt`、`cargo clippy`、`cargo test` 和完整 `pytest evaluation`；LoCoMo mem0 后端的外部依赖可作为可选集成测试处理。

中期：

- 将 benchmark dashboard 改成读取 `evaluation/baselines/index.jsonl`，而不是要求手动传多个 run path。
- 当 dashboard 或制品系统开始读取 `evaluation/baselines/index.jsonl` 时，再把 baseline index 升级为带 `schema_version`、`created_at`、`pipeline_version`、`run_status` 和 `notes` 的正式 schema。
- 先实现确定性 chunk 层，再接入结构化 memory extraction，最后增加 temporal rerank/timeline-aware reasoning，避免一次性改动存储、检索、评分三条链路。
