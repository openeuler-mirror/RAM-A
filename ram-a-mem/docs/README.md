# Documentation

This directory contains source-controlled documentation intended for the formal RAM-A
repository.

## Current Docs

- [benchmarks/prepared-schema-v1.md](benchmarks/prepared-schema-v1.md): unified prepared
  dataset schema for benchmark adapters.
- [design/memory-pipeline-roadmap.md](design/memory-pipeline-roadmap.md): roadmap for
  chunking, semantic memory extraction, and timeline-aware memory reasoning.
- [design/graph-memory-ingestion.md](design/graph-memory-ingestion.md): graph memory
  add, ingestion run, and record embedding stage reference.
- [design/graph-memory-extraction.md](design/graph-memory-extraction.md): graph memory
  structured candidate extraction stage reference.
- [design/graph-memory-resolution.md](design/graph-memory-resolution.md): graph memory
  deterministic resolution and formal graph materialization stage reference.
- [design/graph-memory-retrieval.md](design/graph-memory-retrieval.md): graph memory
  graph seed retrieval, Entity-Fact expansion, ContextBundle assembly, and optional
  MemoryManager search graph channel reference.
- [design/sqlite-hybrid-search.md](design/sqlite-hybrid-search.md): SQLite storage and
  dense/BM25/hybrid retrieval reference.
- [design/memory-cases-storage-split.md](design/memory-cases-storage-split.md): `memory-cases`
  business DB and document/vector index split, table ownership, and index boundaries.
- [guides/locomo-evaluation.md](guides/locomo-evaluation.md): LoCoMo execution guide and
  output reference.
- [guides/memory-cases-manual-quick-verify.md](guides/memory-cases-manual-quick-verify.md):
  `quick_start_verify.sh` usage for the `memory-cases` quick start and verification flow.
- [guides/memory-cases-qa-evaluation.md](guides/memory-cases-qa-evaluation.md): QA eval
  test flow, case schema, coverage, strengths and limitations, and comparison with
  external retrieval tests.
- [guides/ram-a-mem-configuration-and-pipeline.zh-CN.md](guides/ram-a-mem-configuration-and-pipeline.zh-CN.md):
  RAM-A-MEM 完整配置、摄入七阶段、完整摄入链和检索链的数据契约。
- [guides/ram-a-mem-configuration-reference.zh-CN.md](guides/ram-a-mem-configuration-reference.zh-CN.md):
  RAM-A-MEM 全量配置字段、默认值、推荐值、条件依赖和测试要求。
- [guides/ram-a-mem-rpm-agent-self-test.zh-CN.md](guides/ram-a-mem-rpm-agent-self-test.zh-CN.md):
  在 openEuler 容器中安装 RAM-A RPM，并通过裸 MCP 和 xiaoO 验证摄入、检索与持久化。
- [guides/model-compatibility.zh-CN.md](guides/model-compatibility.zh-CN.md):
  Extract/Ground 模型协议、thinking 控制、Structured Output、JSON 修复与 token 预算兼容指南。
- [guides/xiaoo-case-library-integration.md](guides/xiaoo-case-library-integration.md):
  `memory_case_search` deployment, authorization, tool selection, and xiaoO
  integration boundary.

One-off review notes, assistant context, generated reports, and artifact manifests are
intentionally excluded from the formal documentation set.
