#!/usr/bin/env bash
set -euo pipefail

TEST_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PROJECT_ROOT=$(CDPATH= cd -- "$TEST_DIR/.." && pwd)
PYTHON_BIN=${PYTHON_BIN:-python3}

cd "$PROJECT_ROOT"
export PYTHONDONTWRITEBYTECODE=1
export PYTHONPATH="$PROJECT_ROOT/src${PYTHONPATH:+:$PYTHONPATH}"
exec "$PYTHON_BIN" "$TEST_DIR/e2e_mock_vllm.py" "$@"
