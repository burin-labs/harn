#!/usr/bin/env bash
# Step-judge experiment driver.
#
# Invokes `harn eval coding-agent` once per cell × replicate, with the
# right --step-judge / --structural-validator / --model / --tool-format
# / --run-label flags. The aggregator (aggregate.harn) reads every
# invocation's summary.json and produces REPORT.md.
#
# Cells (see experiments/step-judge/README.md):
#   baseline-cheap     | Haiku 4.5  | (no judge)
#   symmetric-cheap    | Haiku 4.5  | Haiku 4.5 judge
#   asymmetric         | Haiku 4.5  | Sonnet 4.6 judge
#   symmetric-strong   | Sonnet 4.6 | Sonnet 4.6 judge

set -euo pipefail

REPLICATES=3
MOCK=0
SINGLE_CELL=""
PROBES=1
ENV_FILE="${HOME}/projects/burin-code/.env"
TOOL_FORMAT="text"
STEP_JUDGE_OVERRIDE=""
STRUCTURAL_VALIDATOR_OVERRIDE=""
OUTPUT_DIR=""
BASELINE_RESULTS_DIR=""
HARN_BIN="${HARN_BIN:-}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --mock)            MOCK=1; shift ;;
    --replicates)      REPLICATES="$2"; shift 2 ;;
    --cell)            SINGLE_CELL="$2"; shift 2 ;;
    --no-probes)       PROBES=0; shift ;;
    --env-file)        ENV_FILE="$2"; shift 2 ;;
    --tool-format)     TOOL_FORMAT="$2"; shift 2 ;;
    --step-judge)      STEP_JUDGE_OVERRIDE="$2"; shift 2 ;;
    --structural-validator) STRUCTURAL_VALIDATOR_OVERRIDE="$2"; shift 2 ;;
    --output-dir)      OUTPUT_DIR="$2"; shift 2 ;;
    --baseline-results-dir) BASELINE_RESULTS_DIR="$2"; shift 2 ;;
    --harn-bin)        HARN_BIN="$2"; shift 2 ;;
    -h|--help)
      sed -n '2,20p' "$0"
      exit 0 ;;
    *)
      echo "unknown flag: $1" >&2
      exit 2 ;;
  esac
done

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TS="$(date +%Y%m%d-%H%M%S)"
OUT_ROOT="${OUTPUT_DIR:-${REPO_ROOT}/experiments/step-judge/results/${TS}}"
mkdir -p "${OUT_ROOT}"

HARN="${HARN_BIN:-${REPO_ROOT}/target/debug/harn}"
if [[ ! -x "${HARN}" ]]; then
  echo "harn binary not found at ${HARN}; run \`cargo build --bin harn\` first" >&2
  exit 1
fi

if [[ "${MOCK}" -eq 1 ]]; then
  CHEAP_MODEL="mock:mock"
  STRONG_MODEL="mock:mock"
  ENV_ARG=()
else
  CHEAP_MODEL="openrouter:anthropic/claude-haiku-4-5"
  STRONG_MODEL="openrouter:anthropic/claude-sonnet-4-6"
  ENV_ARG=("--env-file" "${ENV_FILE}")
fi

run_cell() {
  local label="$1"
  local baseline_label="$2"
  local model="$3"
  local step_judge="$4"
  local extra_args=("${@:5}")
  local cell_dir="${OUT_ROOT}/${label}"
  local effective_step_judge="${STEP_JUDGE_OVERRIDE:-${step_judge}}"
  local baseline_args=()
  local structural_validator_args=()
  mkdir -p "${cell_dir}"
  if [[ -n "${BASELINE_RESULTS_DIR}" ]]; then
    local baseline_summary="${BASELINE_RESULTS_DIR}/${baseline_label}/summary.json"
    if [[ -f "${baseline_summary}" ]]; then
      baseline_args=("--baseline-comparison-against" "${baseline_summary}")
    else
      echo "warning: baseline summary not found for ${baseline_label}: ${baseline_summary}" >&2
    fi
  fi
  if [[ -n "${STRUCTURAL_VALIDATOR_OVERRIDE}" ]]; then
    structural_validator_args=(
      "--structural-validator"
      "${STRUCTURAL_VALIDATOR_OVERRIDE}"
    )
  fi
  echo "── cell: ${label}  model=${model}  tool_format=${TOOL_FORMAT}  step_judge=${effective_step_judge}  structural_validator=${STRUCTURAL_VALIDATOR_OVERRIDE:-default}"
  "${HARN}" eval coding-agent \
    --output "${cell_dir}" \
    --model "${model}" \
    --tool-format "${TOOL_FORMAT}" \
    --max-iterations 8 \
    --run-label "${label}" \
    --step-judge "${effective_step_judge}" \
    "${ENV_ARG[@]}" \
    "${baseline_args[@]}" \
    "${structural_validator_args[@]}" \
    "${extra_args[@]}" \
    || echo "  ! cell ${label} exited with non-zero status (continuing)"
}

declare -a CELLS
if [[ -n "${SINGLE_CELL}" ]]; then
  CELLS=("${SINGLE_CELL}")
else
  CELLS=(
    "baseline-cheap"
    "symmetric-cheap"
    "asymmetric"
    "symmetric-strong"
  )
fi

for replicate in $(seq 1 "${REPLICATES}"); do
  for cell in "${CELLS[@]}"; do
    if [[ "${REPLICATES}" -eq 1 ]]; then
      label="${cell}"
    else
      label="${cell}-r${replicate}"
    fi
    case "${cell}" in
      baseline-cheap|baseline-native)
        run_cell "${label}" "baseline-native" "${CHEAP_MODEL}"  "none" ;;
      symmetric-cheap|symmetric-cheap-native)
        run_cell "${label}" "symmetric-cheap-native" "${CHEAP_MODEL}"  "symmetric-cheap" ;;
      asymmetric|asymmetric-native)
        run_cell "${label}" "asymmetric-native" "${CHEAP_MODEL}"  "asymmetric" ;;
      symmetric-strong|symmetric-strong-native)
        run_cell "${label}" "symmetric-strong-native" "${STRONG_MODEL}" "symmetric-strong" ;;
      *)
        echo "unknown cell: ${cell}" >&2
        exit 2 ;;
    esac
  done
done

if [[ "${PROBES}" -eq 1 && -z "${SINGLE_CELL}" ]]; then
  echo "── probes (run against asymmetric as best-guess winning cell)"
  run_cell "probe-rubric-adversarial" "asymmetric-native" "${CHEAP_MODEL}" "asymmetric" --step-judge-adversarial
  run_cell "probe-transcript-shape-retain" "asymmetric-native" "${CHEAP_MODEL}" "asymmetric" --step-judge-on-veto retain
fi

echo "── aggregating"
"${HARN}" run "${REPO_ROOT}/experiments/step-judge/aggregate.harn" -- \
  --results-dir "${OUT_ROOT}" \
  --output "${OUT_ROOT}/REPORT.md"

echo "── done"
echo "results: ${OUT_ROOT}"
echo "report:  ${OUT_ROOT}/REPORT.md"
