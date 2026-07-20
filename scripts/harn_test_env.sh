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

export HARN_LLM_CALLS_DISABLED=1
export HARN_SESSION_STORE_ROOT="$session_store_root"

"$@"
