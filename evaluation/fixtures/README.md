# Evaluation Fixtures

Small checked-in datasets used for smoke tests and local documentation examples.

- `sample.json`: minimal generic add/search fixture for `memory-bench`.
- `personalmem_sample.json`: minimal PersonaMem-style fixture.
- `locomo_sample.json`: tiny synthetic LoCoMo-style fixture for smoke tests and local examples.

Large raw datasets and prepared full benchmark files should not live in `evaluation/fixtures/`.
If they are checked in under `data/`, include provenance and redistribution notes next to
the file; otherwise keep them in external artifact storage.
