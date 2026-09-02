#!/usr/bin/env bash
set -euo pipefail

curl --fail --silent --show-error http://127.0.0.1:18081/healthy >/dev/null
curl --fail --silent --show-error http://127.0.0.1:18081/ready >/dev/null
