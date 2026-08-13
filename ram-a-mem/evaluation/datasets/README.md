# Dataset Placement

This directory documents dataset conventions. Full benchmark datasets are kept out of
Git and downloaded locally under `data/`.

Use these locations locally:

```text
data/personalmem/raw/          # downloaded PersonaMem CSV/JSONL files
data/personalmem/prepared/     # prepared PersonaMem JSON files
data/longmemeval/              # downloaded LongMemEval files
data/locomo/                   # downloaded LoCoMo file plus README/provenance notes
```

Small fixtures that are safe to commit belong in `evaluation/fixtures/`.
Do not commit downloaded full benchmark files under `data/`.

For any future benchmark dataset, document:

- upstream project or download URL
- upstream license and redistribution constraints
- SHA256 checksum of the exact file used for evaluation, when available
- expected local path
