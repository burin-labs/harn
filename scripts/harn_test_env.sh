#!/usr/bin/env bash

set -euo pipefail

if (( $# == 0 )); then
  echo "usage: harn_test_env.sh command [args ...]" >&2
  exit 2
fi

# Harn tests intentionally configure their own egress policy. Keep host
# configuration from changing those tests' meaning, and give each invocation
# a fresh durable session store so fixed fixture session IDs cannot resume a
# transcript left by another test process.
unset \
  HARN_EGRESS_ALLOW \
  HARN_EGRESS_DENY \
  HARN_EGRESS_DEFAULT \
  HARN_EGRESS_BLOCK_PRIVATE \
  HARN_EGRESS_ALLOW_LOOPBACK \
  HARN_SESSION_STORE_ROOT

session_store_root="$(mktemp -d "${TMPDIR:-/tmp}/harn-test-session.XXXXXX")"
trap 'rm -rf -- "$session_store_root"' EXIT

child_pid=""
# Invoked indirectly from the signal traps below.
# shellcheck disable=SC2329
forward_signal() {
  local signal="$1"
  local status="$2"
  if [[ -n "$child_pid" ]] && kill -0 "$child_pid" 2>/dev/null; then
    kill -s "$signal" "$child_pid" 2>/dev/null || true
    wait "$child_pid" 2>/dev/null || true
  fi
  exit "$status"
}
trap 'forward_signal HUP 129' HUP
trap 'forward_signal INT 130' INT
trap 'forward_signal TERM 143' TERM

export HARN_LLM_CALLS_DISABLED=1
export HARN_SESSION_STORE_ROOT="$session_store_root"

"$@" &
child_pid=$!
if wait "$child_pid"; then
  status=0
else
  status=$?
fi
child_pid=""
exit "$status"
