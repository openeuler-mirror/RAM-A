# Dataset Placement

This directory documents dataset conventions. Full benchmark datasets are normally kept
out of `evaluation/fixtures/`; if a benchmark file is checked in, keep it under `data/`
with explicit provenance, license or redistribution notes, and a checksum.

Use these locations locally:

```text
data/personalmem/raw/          # downloaded PersonaMem CSV/JSONL files
data/personalmem/prepared/     # prepared PersonaMem JSON files
data/longmemeval/              # downloaded LongMemEval files
data/locomo/                   # LoCoMo benchmark file and provenance notes
```

Small fixtures that are safe to commit belong in `evaluation/fixtures/`.

For any future checked-in benchmark file under `data/`, document:

- upstream project or download URL
- upstream license and redistribution constraints
- SHA256 checksum of the exact file used for evaluation
- whether the file may be redistributed with the formal repository
