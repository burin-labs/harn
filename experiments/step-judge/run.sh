#!/usr/bin/env bash
# Step-judge experiment driver.
#
# Invokes `harn eval coding-agent` once per cell × replicate, with the
# right --step-judge / --model / --run-label flags. The aggregator
# (aggregate.harn) reads every invocation's summary.json and produces
# REPORT.md.
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

while [[ $# -gt 0 ]]; do
  case "$1" in
    --mock)            MOCK=1; shift ;;
    --replicates)      REPLICATES="$2"; shift 2 ;;
    --cell)            SINGLE_CELL="$2"; shift 2 ;;
    --no-probes)       PROBES=0; shift ;;
    --env-file)        ENV_FILE="$2"; shift 2 ;;
    -h|--help)
      sed -n '2,15p' "$0"
      exit 0 ;;
    *)
      echo "unknown flag: $1" >&2
      exit 2 ;;
  esac
done

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TS="$(date +%Y%m%d-%H%M%S)"
OUT_ROOT="${REPO_ROOT}/experiments/step-judge/results/${TS}"
mkdir -p "${OUT_ROOT}"

HARN="${REPO_ROOT}/target/debug/harn"
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
  local model="$2"
  local step_judge="$3"
  local extra_args=("${@:4}")
  local cell_dir="${OUT_ROOT}/${label}"
  mkdir -p "${cell_dir}"
  echo "── cell: ${label}  model=${model}  step_judge=${step_judge}"
  "${HARN}" eval coding-agent \
    --output "${cell_dir}" \
    --model "${model}" \
    --tool-format "text" \
    --max-iterations 8 \
    --run-label "${label}" \
    --step-judge "${step_judge}" \
    "${ENV_ARG[@]}" \
    "${extra_args[@]}" \
    || echo "  ! cell ${label} exited with non-zero status (continuing)"
}

declare -a CELLS
if [[ -n "${SINGLE_CELL}" ]]; then
  CELLS=("${SINGLE_CELL}")
else
  CELLS=("baseline-cheap" "symmetric-cheap" "asymmetric" "symmetric-strong")
fi

for replicate in $(seq 1 "${REPLICATES}"); do
  for cell in "${CELLS[@]}"; do
    label="${cell}-r${replicate}"
    case "${cell}" in
      baseline-cheap)
        run_cell "${label}" "${CHEAP_MODEL}"  "none" ;;
      symmetric-cheap)
        run_cell "${label}" "${CHEAP_MODEL}"  "symmetric-cheap" ;;
      asymmetric)
        run_cell "${label}" "${CHEAP_MODEL}"  "asymmetric" ;;
      symmetric-strong)
        run_cell "${label}" "${STRONG_MODEL}" "symmetric-strong" ;;
      *)
        echo "unknown cell: ${cell}" >&2
        exit 2 ;;
    esac
  done
done

if [[ "${PROBES}" -eq 1 && -z "${SINGLE_CELL}" ]]; then
  echo "── probes (run against asymmetric as best-guess winning cell)"
  run_cell "probe-rubric-adversarial" "${CHEAP_MODEL}" "asymmetric" --step-judge-adversarial
  run_cell "probe-transcript-shape-retain" "${CHEAP_MODEL}" "asymmetric" --step-judge-on-veto retain
fi

echo "── aggregating"
"${HARN}" run "${REPO_ROOT}/experiments/step-judge/aggregate.harn" -- \
  --results-dir "${OUT_ROOT}" \
  --output "${OUT_ROOT}/REPORT.md"

echo "── done"
echo "results: ${OUT_ROOT}"
echo "report:  ${OUT_ROOT}/REPORT.md"
