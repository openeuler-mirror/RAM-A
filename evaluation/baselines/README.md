# Baseline Results

Keep full benchmark artifacts out of Git. Store only a small index record in this directory
and put large files in object storage, release assets, or another artifact system.

Recommended files:

- `index.jsonl`: one JSON object per benchmark run.
- `index.example.jsonl`: schema example that can be copied when creating `index.jsonl`.

Each record should include enough metadata to compare version 1 and version 2 runs without
opening the large raw artifacts.

For the first import, `index.example.jsonl` is intentionally a lightweight index template,
not a strict schema. It is enough for basic version-to-version comparison as long as each
run records the code ref, dataset checksum, model settings, retrieval settings, compact
metrics, and external artifact location.

Required fields:

- `run_id`: stable run identifier, usually the output directory name.
- `dataset`: benchmark key such as `personamem`, `longmemeval`, or `locomo`.
- `split`: dataset split or fixture name.
- `backend`: `RAM-A`, `mem0`, `memos`, or another comparable backend key.
- `code_ref`: commit SHA or branch name used for the run.
- `dataset_sha256`: checksum of the exact input dataset or prepared dataset.
- `embedding`: provider, model, and dimensions.
- `retrieval`: top-k, candidate-k, search mode, and fusion weights.
- `answer_model` and `judge_model`: model names when the run includes QA.
- `metrics`: compact summary metrics used for comparisons.
- `artifact_uri`: external location for raw results and HTML reports.

Full artifacts should include `run_meta.json`, raw search results, answer or judge results,
metrics JSON, and HTML reports. Compress large JSON files before uploading them.

If the dashboard starts reading `index.jsonl` directly, or if RAM-A needs stricter
v1/v2 comparison governance, extend the record later with fields such as `schema_version`,
`created_at`, `pipeline_version`, `run_status`, and `notes`.
