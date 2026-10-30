#!/usr/bin/env bash
set -euo pipefail

report_duration_budget() {
  local duration=$1 limit_minutes=$2 memory_mb=$3
  local warning_percent=80
  if [[ ! "$duration" =~ ^[0-9]+$ || ! "$limit_minutes" =~ ^[1-9][0-9]*$ ||
        ! "$memory_mb" =~ ^[1-9][0-9]*$ ]]; then
    echo "::warning::RUST_TEST_DURATION_UNMEASURED duration_seconds=$duration job_limit_minutes=$limit_minutes memory_mb=$memory_mb"
    return
  fi
  local limit_seconds=$((limit_minutes * 60))
  local warning_seconds=$((limit_seconds * warning_percent / 100))
  local warning_label="warning threshold ${warning_percent}%"
  local feedback_seconds=${4:-}
  if [[ -n "$feedback_seconds" ]]; then
    if [[ ! "$feedback_seconds" =~ ^[1-9][0-9]*$ ]] || ((feedback_seconds > limit_seconds)); then
      echo "::warning::RUST_TEST_DURATION_UNMEASURED duration_seconds=$duration job_limit_minutes=$limit_minutes feedback_budget_seconds=$feedback_seconds"
      return
    fi
    warning_seconds=$feedback_seconds
    warning_label="feedback budget ${feedback_seconds}s"
  fi
  local message="Rust build and test lane: ${duration}s of ${limit_seconds}s job limit; ${warning_label}; runner memory ${memory_mb} MiB."
  echo "$message"
  if ((duration > warning_seconds)); then
    echo "::warning::$message"
  fi
  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    printf '\n%s\n' "$message" >> "$GITHUB_STEP_SUMMARY"
  fi
}

# The owning reporter accepts measured values so its threshold controls need
# neither long-running test commands nor a fake clock.
if [[ "${BASH_SOURCE[0]}" != "$0" ]]; then
  return
fi

if [[ $# -eq 0 ]]; then
  echo "usage: $0 <test command> [args...]" >&2
  exit 2
fi

# Run the tests in the one test environment `make test` also uses
# (scripts/harn_test_env.sh): the 16 MiB thread stack the production CLI uses,
# no ambient egress policy or config, an empty user config directory, and a
# fresh session store. This wrapper only adds CI resource reporting.
test_env="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/harn_test_env.sh"

report_resources() {
  local label="$1"

  echo "::group::$label"
  if command -v nproc >/dev/null 2>&1; then
    echo "nproc=$(nproc)"
  fi
  print_memory
  df -h . || true
  echo "::endgroup::"

  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    {
      echo "### $label"
      echo
      if command -v nproc >/dev/null 2>&1; then
        echo "- \`nproc=$(nproc)\`"
      fi
      echo
      echo '```text'
      print_memory
      echo
      df -h . || true
      echo '```'
      echo
    } >> "$GITHUB_STEP_SUMMARY"
  fi
}

print_memory() {
  if command -v free >/dev/null 2>&1; then
    free -m || true
  elif command -v vm_stat >/dev/null 2>&1; then
    vm_stat || true
  else
    echo "memory report unavailable"
  fi
}

# shellcheck disable=SC2329 # invoked by traps below
report_on_signal() {
  local signal="$1"
  report_resources "Rust test resources after ${signal}"
  exit 143
}

trap 'report_on_signal SIGTERM' TERM
trap 'report_on_signal SIGINT' INT

report_resources "Rust test resources before"
memory_mb=unmeasured
if [[ -n "${RUST_TEST_JOB_LIMIT_MINUTES:-}" ]]; then
  source "$(dirname "$test_env")/ci/rust_resource_budget.sh"
  memory_mb=$(host_memory_mb) || memory_mb=unmeasured
fi
started=$SECONDS
status=0
if [[ -n "${RUST_TEST_STDOUT_PATH:-}" ]]; then
  bash "$test_env" --per-test-state "$@" >"$RUST_TEST_STDOUT_PATH" || status=$?
else
  bash "$test_env" --per-test-state "$@" || status=$?
fi
duration=$((SECONDS - started))
if [[ -n "${RUST_TEST_JOB_LIMIT_MINUTES:-}" ]]; then
  report_duration_budget "$duration" "$RUST_TEST_JOB_LIMIT_MINUTES" "$memory_mb" "${RUST_TEST_FEEDBACK_BUDGET_SECONDS:-}"
fi
report_resources "Rust test resources after"
echo "rust_test_lane_seconds=${duration}"
if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  {
    echo "### Rust build and test lane"
    echo
    echo "- Duration: ${duration}s"
    echo "- Exit status: ${status}"
  } >> "$GITHUB_STEP_SUMMARY"
fi
exit "$status"
