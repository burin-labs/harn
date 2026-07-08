#!/usr/bin/env bash
set -euo pipefail

if [[ $# -eq 0 ]]; then
  echo "usage: $0 <test command> [args...]" >&2
  exit 2
fi

: "${CARGO_BUILD_JOBS:=4}"
export CARGO_BUILD_JOBS

report_resources() {
  local label="$1"

  echo "::group::$label"
  echo "CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}"
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
      echo "- \`CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}\`"
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
status=0
"$@" || status=$?
report_resources "Rust test resources after"
exit "$status"
