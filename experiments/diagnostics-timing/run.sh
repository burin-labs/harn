#!/usr/bin/env bash
# Sweep the diagnostics-timing arms across fixtures.
#
#   ./run.sh --mode mock   --out runs/mock.jsonl
#   ./run.sh --mode live   --llm ollama:qwen2.5-coder --trials 3 --out runs/stage1.jsonl
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

mode="mock"
llm=""
trials=1
out="runs/sweep.jsonl"
arms="A-push-all B-verify-time C-settle D-pull E-hybrid F-hybrid-batch"
fixtures="rename-single-file rename-cross-file genuine-typo dynamic-runtime-names clean-control"
max_iter=20

while [[ $# -gt 0 ]]; do
  case "$1" in
    --mode) mode="$2"; shift 2 ;;
    --llm) llm="$2"; shift 2 ;;
    --trials) trials="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    --arms) arms="$2"; shift 2 ;;
    --fixtures) fixtures="$2"; shift 2 ;;
    --max-iter) max_iter="$2"; shift 2 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

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
      EXP_OUT="$out" \
      EXP_MAX_ITER="$max_iter" \
        $harn_cmd playground \
          --host host.harn \
          --script rig.harn \
          --task "diagnostics-timing" \
          "${llm_args[@]}" \
          >/dev/null 2>&1 || echo "   trial errored" >&2
    done
  done
done

echo "wrote $out" >&2
wc -l < "$out" >&2
