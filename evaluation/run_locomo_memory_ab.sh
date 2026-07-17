#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
PROJECT_ROOT="$(CDPATH= cd -- "${SCRIPT_DIR}/.." && pwd)"
cd "$SCRIPT_DIR"

if [ -f .env ]; then
    set -a
    . ./.env
    set +a
fi

: "${OPENROUTER_API_KEY:?OPENROUTER_API_KEY must be set}"
PHASE="${PHASE:-pilot}"
PYTHON_BIN="${PYTHON_BIN:-python3}"
DATASET="${DATASET:-${PROJECT_ROOT}/data/locomo/locomo10.json}"
RUN_DIR="${RUN_DIR:-outputs/locomo-memory-ab/${PHASE}}"
PREFLIGHT_PATH="${RUN_DIR}/preflight.json"
export PREFLIGHT_PATH

case "$PHASE" in
    pilot)
        ;;
    full)
        : "${FROZEN_CONFIG:?FROZEN_CONFIG is required for a full run}"
        export FROZEN_CONFIG
        ;;
    *)
        echo "PHASE must be pilot or full" >&2
        exit 2
        ;;
esac

"$PYTHON_BIN" locomo/locomo_preflight.py --output "$PREFLIGHT_PATH"

MEMORY_MODE=raw "$PYTHON_BIN" locomo/locomo_run.py \
    --phase "$PHASE" --dataset "$DATASET" --run-dir "$RUN_DIR/raw"

MEMORY_MODE=extracted "$PYTHON_BIN" locomo/locomo_run.py \
    --phase "$PHASE" --dataset "$DATASET" --run-dir "$RUN_DIR/extracted"

"$PYTHON_BIN" locomo/locomo_compare.py \
    --phase "$PHASE" \
    --raw-dir "$RUN_DIR/raw" \
    --treatment-dir "$RUN_DIR/extracted" \
    --output-json "$RUN_DIR/comparison.json" \
    --html-report "$RUN_DIR/comparison.html"

echo "[done] LoCoMo memory A/B ${PHASE} artifacts: ${RUN_DIR}"
