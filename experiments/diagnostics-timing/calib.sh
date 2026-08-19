#!/usr/bin/env bash
# One calibration trial with LLM transcript capture, for inspecting exactly what
# the model emitted when the rig's tool loop stalls.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here"

export LLAMACPP_BASE_URL=${LLAMACPP_BASE_URL:-http://tornadough.local:8001}
export HARN_LLM_TRANSCRIPT_DIR=${HARN_LLM_TRANSCRIPT_DIR:-/tmp/diagcalib-llm}
export HARN_BIN_NO_BUILD=1
export EXP_RIG_SHA
EXP_RIG_SHA=$(git -C "$here" rev-parse --short HEAD)

rm -rf "$HARN_LLM_TRANSCRIPT_DIR"
mkdir -p "$HARN_LLM_TRANSCRIPT_DIR"

EXP_ARM=${1:-A-push-all} \
EXP_FIXTURE=${2:-rename-single-file} \
EXP_OUT=${3:-runs/calib-debug.jsonl} \
EXP_MAX_ITER=${EXP_MAX_ITER:-15} \
  "$here/../../scripts/harn_bin.sh" -- playground \
    --host host.harn \
    --script rig.harn \
    --task calib \
    --llm llamacpp:qwen3.6-35b-a3b-ud-q4-k-xl

echo "--- transcript files ---" >&2
ls -la "$HARN_LLM_TRANSCRIPT_DIR" >&2
