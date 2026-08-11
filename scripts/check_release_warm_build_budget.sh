#!/usr/bin/env bash
# Compare one release Build-step wall time against the closed warm-build budget.
#
# Only Build-step seconds count. Queue time and skipped targets are ignored.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
POLICY="${HARN_RELEASE_WARM_BUILD_BUDGET:-${ROOT}/.github/release-warm-build-budget.json}"
TARGET=""
DURATION=""
MODE="warm"
ENFORCE="warn"

usage() {
  cat <<'EOF'
Usage: scripts/check_release_warm_build_budget.sh --target TARGET --duration SECONDS [--mode MODE] [--enforce warn|fail]

MODE is warm, candidate, primary, recovery, or benchmark. Enforcement applies to
warm and candidate only; other modes always report without failing.
EOF
}

while (( $# > 0 )); do
  case "$1" in
    --target)
      TARGET="${2:-}"
      shift 2
      ;;
    --duration)
      DURATION="${2:-}"
      shift 2
      ;;
    --mode)
      MODE="${2:-}"
      shift 2
      ;;
    --enforce)
      ENFORCE="${2:-}"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      printf 'unknown argument: %s\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "$TARGET" || -z "$DURATION" ]]; then
  usage >&2
  exit 2
fi
if ! [[ "$DURATION" =~ ^[0-9]+$ ]]; then
  echo "duration must be a non-negative integer number of seconds" >&2
  exit 2
fi
case "$ENFORCE" in
  warn|fail) ;;
  *)
    echo "--enforce must be warn or fail" >&2
    exit 2
    ;;
esac

if [[ ! -f "$POLICY" ]]; then
  echo "missing warm-build budget policy: $POLICY" >&2
  exit 2
fi

mapfile -t budget_lines < <(jq -r -f "${ROOT}/.github/release-warm-build-budget.jq" "$POLICY")
entry="$(jq -c --arg target "$TARGET" '.targets[] | select(.target == $target)' "$POLICY")"
if [[ -z "$entry" ]]; then
  echo "warm-build budget has no target '$TARGET'" >&2
  exit 2
fi

baseline="$(jq -r '.baseline_seconds' <<<"$entry")"
warn_at="$(jq -r '.warn_seconds' <<<"$entry")"
budget="$(jq -r '.budget_seconds' <<<"$entry")"
baseline_run="$(jq -r '.baseline.run_id' "$POLICY")"
baseline_version="$(jq -r '.baseline.version' "$POLICY")"

{
  echo "### Release warm-build budget"
  echo
  echo "| Field | Value |"
  echo "| --- | --- |"
  echo "| Target | $TARGET |"
  echo "| Mode | $MODE |"
  echo "| Build-step duration | ${DURATION}s |"
  echo "| Baseline (v${baseline_version} run ${baseline_run}) | ${baseline}s |"
  echo "| Warn at | ${warn_at}s |"
  echo "| Budget | ${budget}s |"
  echo "| Enforce | $ENFORCE |"
} >> "${GITHUB_STEP_SUMMARY:-/dev/null}"

status="ok"
if (( DURATION > budget )); then
  status="over_budget"
elif (( DURATION >= warn_at )); then
  status="warn"
fi

echo "target=$TARGET"
echo "duration_seconds=$DURATION"
echo "baseline_seconds=$baseline"
echo "warn_seconds=$warn_at"
echo "budget_seconds=$budget"
echo "status=$status"
echo "policy_targets=${#budget_lines[@]}"

case "$MODE" in
  warm|candidate)
    ;;
  *)
    echo "mode $MODE is informational only; not enforcing warm-build budget"
    exit 0
    ;;
esac

if [[ "$status" == "ok" ]]; then
  exit 0
fi

message="${TARGET} Build step took ${DURATION}s against warm budget ${budget}s (baseline ${baseline}s from v${baseline_version})."
if [[ "$status" == "warn" ]]; then
  echo "::warning title=Release warm-build budget::$message"
  exit 0
fi

if [[ "$ENFORCE" == "fail" ]]; then
  echo "::error title=Release warm-build budget::$message"
  exit 1
fi

echo "::warning title=Release warm-build budget::$message"
exit 0
