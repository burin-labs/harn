#!/usr/bin/env bash
# One calibration trial with LLM transcript capture, for inspecting exactly what
# the model emitted when the rig's tool loop stalls.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here"

# Pinned, never inherited: the same port commonly serves a DIFFERENT model on
# localhost, so an ambient LLAMACPP_BASE_URL silently swaps the box under the
# measurement. Require an explicit, differently-named variable so nothing can be
# picked up by accident. No trailing /v1.
: "${DIAG_TIMING_BASE_URL:?set DIAG_TIMING_BASE_URL to the pinned server, e.g. http://server.example:8001}"
export LLAMACPP_BASE_URL="$DIAG_TIMING_BASE_URL"
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
