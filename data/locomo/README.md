# LoCoMo Data

This directory is the local placement target for LoCoMo benchmark data.

The full `locomo10.json` file is not committed to the RAM-A repository. Download it
locally before running the full LoCoMo pipeline:

```bash
mkdir -p data/locomo
curl -L https://raw.githubusercontent.com/snap-research/locomo/main/data/locomo10.json \
  -o data/locomo/locomo10.json
```

The checked-in smoke fixture remains under `evaluation/fixtures/locomo_sample.json`.

## Provenance

- Source benchmark: LoCoMo, *Evaluating Very Long-Term Conversational Memory of LLM Agents* (ACL 2024)
- Upstream project: https://github.com/snap-research/locomo
- Upstream data path: `data/locomo10.json`
- Direct data URL: https://raw.githubusercontent.com/snap-research/locomo/main/data/locomo10.json
- Expected local path: `data/locomo/locomo10.json`
- Expected SHA256 after download: `79FA87E90F04081343B8C8DEBECB80A9A6842B76A7AA537DC9FDF651EA698FF4`

## License / Redistribution

- Upstream license: Creative Commons Attribution-NonCommercial 4.0 International (CC BY-NC 4.0)
- License file: https://raw.githubusercontent.com/snap-research/locomo/main/LICENSE.txt
- The NonCommercial restriction applies. Use this dataset for non-commercial research and evaluation unless the upstream rights holder grants additional permission.
- Keep attribution to the LoCoMo authors and cite the paper when publishing results.
- Do not commit the downloaded benchmark JSON to this repository.

## Integrity Check

PowerShell:

```powershell
Get-FileHash data/locomo/locomo10.json -Algorithm SHA256
```

Expected SHA256:

```text
79FA87E90F04081343B8C8DEBECB80A9A6842B76A7AA537DC9FDF651EA698FF4
```
