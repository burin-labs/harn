#!/usr/bin/env bash
# Sweep the diagnostics-timing arms across fixtures.
#
#   ./run.sh --mode mock   --out runs/mock.jsonl
#   ./run.sh --mode live   --llm ollama:qwen2.5-coder --trials 3 --out runs/stage1.jsonl
#
# Live mode pins the inference endpoint (--base-url, default the GPU host) and
# refuses to start unless /v1/models answers 200 AND serves the requested model.
#
# Mock mode replays a scripted trajectory and proves only that the arms deliver
# diagnostics at different times. It cannot measure behaviour: a scripted model
# does not read what it is shown. Every behavioural number needs --mode live.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here"

# The rig reads `resolution` off ast.undefined_names, so it needs a harn built
# from this worktree rather than whatever is on PATH. Override with HARN=... to
# point at a specific binary.
harn_cmd=${HARN:-"$here/../../scripts/harn_bin.sh --"}
export HARN_BIN_NO_BUILD=${HARN_BIN_NO_BUILD:-1}
export EXP_RIG_SHA=${EXP_RIG_SHA:-$(git -C "$here" rev-parse --short HEAD 2>/dev/null || echo unknown)}

mode="mock"
llm=""
trials=1
out="runs/sweep.jsonl"
arms="A-push-all B-verify-time C-settle D-pull E-hybrid F-hybrid-batch"
fixtures="rename-single-file rename-cross-file genuine-typo dynamic-runtime-names clean-control"
max_iter=20
# Pinned, never inherited. The same port serves a DIFFERENT model on this Mac's
# localhost, so an ambient value silently swaps the box under the measurement
# and the run reports a number from a machine nobody chose. calib.sh already
# pins it; the sweep has to pin it too or the calibration and the matrix are not
# measuring the same thing.
base_url="http://tornadough:8001"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --mode) mode="$2"; shift 2 ;;
    --llm) llm="$2"; shift 2 ;;
    --trials) trials="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    --arms) arms="$2"; shift 2 ;;
    --fixtures) fixtures="$2"; shift 2 ;;
    --max-iter) max_iter="$2"; shift 2 ;;
    --base-url) base_url="$2"; shift 2 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

# Live mode preflight. A 200 alone is not enough: the pinned host has to be
# serving the model this sweep asked for, because a reachable server with a
# different model loaded answers 200 and then quietly measures something else.
if [[ "$mode" == "live" ]]; then
  if [[ -z "$llm" ]]; then
    echo "--mode live requires --llm provider:model" >&2
    exit 2
  fi
  export LLAMACPP_BASE_URL="$base_url"
  echo "== preflight: ${base_url}/v1/models" >&2
  models_body=$(mktemp)
  trap 'rm -f "$models_body"' EXIT
  status=$(curl -sS -o "$models_body" -w '%{http_code}' --max-time 20 "${base_url}/v1/models") || true
  status=${status:-000}
  if [[ "$status" != "200" ]]; then
    echo "   preflight FAILED: ${base_url}/v1/models answered ${status}; spending no GPU" >&2
    exit 4
  fi
  wanted_model="${llm#*:}"
  if ! grep -qF "$wanted_model" "$models_body"; then
    echo "   preflight FAILED: ${base_url} answered 200 but does not serve '${wanted_model}'" >&2
    echo "   served: $(tr -d '\n' < "$models_body" | cut -c1-400)" >&2
    exit 4
  fi
  echo "   preflight OK: 200, serving '${wanted_model}'" >&2
fi

mkdir -p "$(dirname "$out")"
: > "$out"

for fixture in $fixtures; do
  for arm in $arms; do
    for trial in $(seq 1 "$trials"); do
      echo "== $fixture / $arm / trial $trial" >&2
      llm_args=()
      if [[ "$mode" == "mock" ]]; then
        mock="fixtures/${fixture}.mock.jsonl"
        if [[ ! -f "$mock" ]]; then
          echo "   no mock fixture for $fixture, skipping" >&2
          continue
        fi
        llm_args=(--llm-mock "$mock")
      else
        if [[ -z "$llm" ]]; then
          echo "--mode live requires --llm provider:model" >&2
          exit 2
        fi
        llm_args=(--llm "$llm")
      fi
      EXP_ARM="$arm" \
      EXP_FIXTURE="$fixture" \
      EXP_TRIAL="$trial" \
      EXP_LLM="$llm" \
      EXP_OUT="$out" \
      EXP_MAX_ITER="$max_iter" \
        $harn_cmd playground \
          --host host.harn \
          --script rig.harn \
          --task "diagnostics-timing" \
          "${llm_args[@]}" \
          2>>"${out%.jsonl}.log" >/dev/null || echo "   trial errored (see ${out%.jsonl}.log)" >&2
    done
  done
done

echo "wrote $out" >&2
wc -l < "$out" >&2
