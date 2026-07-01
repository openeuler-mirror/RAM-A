# LoCoMo Data

This directory contains LoCoMo benchmark data used by the evaluation pipeline.

## Files

- `locomo10.json`: standard LoCoMo 10-conversation benchmark file used by RAM-A and comparable memory-system evaluation scripts.
  - Size: 2,805,274 bytes
  - SHA256: `79FA87E90F04081343B8C8DEBECB80A9A6842B76A7AA537DC9FDF651EA698FF4`

## Provenance

- Source benchmark: LoCoMo, *Evaluating Very Long-Term Conversational Memory of LLM Agents* (ACL 2024)
- Upstream project: https://github.com/snap-research/locomo
- Upstream data path: `data/locomo10.json`

## License / Redistribution

- Upstream license: Creative Commons Attribution-NonCommercial 4.0 International (CC BY-NC 4.0)
- License file: https://raw.githubusercontent.com/snap-research/locomo/main/LICENSE.txt
- The NonCommercial restriction applies. Use this dataset for non-commercial research and evaluation unless the upstream rights holder grants additional permission.
- Keep attribution to the LoCoMo authors and cite the paper when publishing results.
- This file is retained in the first import to match the common `data/locomo/locomo10.json` evaluation convention used by related memory-system projects. If a downstream distribution channel cannot include non-commercial benchmark data, remove this JSON file and fetch it from the upstream project instead.

## Integrity Check

PowerShell:

```powershell
Get-FileHash data/locomo/locomo10.json -Algorithm SHA256
```

Expected SHA256:

```text
79FA87E90F04081343B8C8DEBECB80A9A6842B76A7AA537DC9FDF651EA698FF4
```
