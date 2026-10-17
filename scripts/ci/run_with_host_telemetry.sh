#!/usr/bin/env bash
# Stream host snapshots while a command runs. Earlier snapshots remain in the
# Actions log even when runner shutdown prevents the failure report from firing.
set -euo pipefail

if [[ $# -lt 2 ]]; then
  echo "usage: $0 <label> <command> [args...]" >&2
  exit 2
fi
label=$1
shift
report="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/report_host_state.sh"
owner_pid=$$

snapshot() {
  bash "$report" --during "$label" || echo "host telemetry snapshot failed"
}

# shellcheck disable=SC2329 # invoked in the background below
monitor() {
  local timer_pid=""
  trap 'exit 0' TERM INT
  trap 'if [[ -n "$timer_pid" ]]; then kill "$timer_pid" 2>/dev/null || true; wait "$timer_pid" 2>/dev/null || true; fi' EXIT
  while kill -0 "$owner_pid" 2>/dev/null; do
    sleep 30 &
    timer_pid=$!
    wait "$timer_pid"
    timer_pid=""
    kill -0 "$owner_pid" 2>/dev/null || break
    snapshot
  done
}

# Make the first reading synchronous: even a command that dies immediately
# must leave a resource census. Diagnostics never replace the command's status.
snapshot
monitor &
monitor_pid=$!
trap 'kill "$monitor_pid" 2>/dev/null || true; wait "$monitor_pid" 2>/dev/null || true' EXIT

"$@"
