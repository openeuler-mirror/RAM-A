# Evaluation Fixtures

Small checked-in datasets used for smoke tests and local documentation examples.

- `sample.json`: minimal generic add/search fixture for `memory-bench`.
- `personalmem_sample.json`: minimal PersonaMem-style fixture.
- `locomo_sample.json`: tiny synthetic LoCoMo-style fixture for smoke tests and local examples.

Large raw datasets and prepared full benchmark files should not live in `evaluation/fixtures/`.
Keep them as local downloads under `data/` and include source links plus redistribution
notes in the relevant README.

For PersonaMem graph smoke tests, run
`evaluation/personalmem/prepare_fixture.py` first. It deterministically converts the
small raw fixture into a temporary `benchmark-prepared-v1` file with `scope_id` and
query filters; no full PersonaMem download is required.
