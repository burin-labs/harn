#!/usr/bin/env bash
set -euo pipefail

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
started=$SECONDS
status=0
if [[ -n "${RUST_TEST_STDOUT_PATH:-}" ]]; then
  bash "$test_env" --per-test-state "$@" >"$RUST_TEST_STDOUT_PATH" || status=$?
else
  bash "$test_env" --per-test-state "$@" || status=$?
fi
duration=$((SECONDS - started))
report_resources "Rust test resources after"
echo "rust_test_execution_seconds=${duration}"
if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  {
    echo "### Rust test execution"
    echo
    echo "- Duration: ${duration}s"
    echo "- Exit status: ${status}"
  } >> "$GITHUB_STEP_SUMMARY"
fi
exit "$status"
