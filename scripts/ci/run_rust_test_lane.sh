#!/usr/bin/env bash
set -euo pipefail

if [[ $# -eq 0 ]]; then
  echo "usage: $0 <test command> [args...]" >&2
  exit 2
fi

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
"$@" || status=$?
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
