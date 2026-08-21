#!/usr/bin/env bash
# One calibration trial with LLM transcript capture, for inspecting exactly what
# the model emitted when the rig's tool loop stalls.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here"

# Pinned, never inherited: the same port serves a DIFFERENT model on this Mac's
# localhost (another lane's server), so an ambient value silently swaps the box
# under the measurement. No trailing /v1.
export LLAMACPP_BASE_URL=http://tornadough:8001
# The tool channel comes from the catalog (preferred_tool_format for this
# route), which is the authority on provider/model support: this route pins
# json with tool_mode_parity=text_only from a receipted sweep, and forcing
# native shipped zero tool schemas (4/4 calibration runs at 0 tool calls).
# EXP_TOOL_FORMAT stays available as an explicit channel-experiment override;
# the provenance line records requested, effective, and catalog values.
export EXP_TOOL_FORMAT=${EXP_TOOL_FORMAT-}
export HARN_LLM_TRANSCRIPT_DIR=${HARN_LLM_TRANSCRIPT_DIR:-/tmp/diagcalib-llm}
export HARN_BIN_NO_BUILD=1
export EXP_RIG_SHA
EXP_RIG_SHA=$(git -C "$here" rev-parse --short HEAD)

rm -rf "$HARN_LLM_TRANSCRIPT_DIR"
mkdir -p "$HARN_LLM_TRANSCRIPT_DIR"

llm_selector="llamacpp:qwen3.6-35b-a3b-ud-q4-k-xl"

EXP_ARM=${1:-A-push-all} \
EXP_FIXTURE=${2:-rename-single-file} \
EXP_OUT=${3:-runs/calib-debug.jsonl} \
EXP_MAX_ITER=${EXP_MAX_ITER:-15} \
EXP_LLM="$llm_selector" \
  "$here/../../scripts/harn_bin.sh" -- playground \
    --host host.harn \
    --script rig.harn \
    --task calib \
    --llm "$llm_selector"

echo "--- transcript files ---" >&2
ls -la "$HARN_LLM_TRANSCRIPT_DIR" >&2
