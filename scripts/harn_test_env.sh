#!/usr/bin/env bash

# The one environment every Harn test process runs in: Rust test lanes (make
# and CI), script tests, and conformance. A test must not depend on the shell
# or machine that launched it, so this script removes ambient configuration
# and gives each invocation private, empty state roots. Tests that need a
# setting seed it themselves after startup.

set -euo pipefail

# --per-test-state: the command is a test runner (nextest, cargo test) whose
# tests run as separate processes that each create their own session store and
# state roots. Sharing one store across them turns independent tests into
# SQLite lock contention and cross-talk, so in this mode the ambient value is
# still dropped but no shared store is exported.
per_test_state=0
if [[ "${1:-}" == "--per-test-state" ]]; then
  per_test_state=1
  shift
fi

if (( $# == 0 )); then
  echo "usage: harn_test_env.sh [--per-test-state] command [args ...]" >&2
  exit 2
fi

# Harn tests configure their own egress policy, session store, and provider
# catalog. Drop the host's values for all of them, including explicit config
# file pointers a wrapper may export (a host's HARN_HOST_PROVIDERS_CONFIG, a
# user's HARN_PROVIDERS_CONFIG), so none can change a test's meaning.
unset \
  HARN_EGRESS_ALLOW \
  HARN_EGRESS_DENY \
  HARN_EGRESS_DEFAULT \
  HARN_EGRESS_BLOCK_PRIVATE \
  HARN_EGRESS_ALLOW_LOOPBACK \
  HARN_SESSION_STORE_ROOT \
  HARN_CONFIG_USER \
  HARN_CONFIG_MANAGED \
  HARN_PROVIDERS_CONFIG \
  HARN_HOST_PROVIDERS_CONFIG \
  HARN_LLM_PROVIDER \
  HARN_LLM_MODEL \
  HARN_DEFAULT_PROVIDER \
  HARN_MCP_PRESETS_CONFIG \
  HARN_MCP_BULK_AUTH_CONFIG

# Keep tests off the login keychain unless the caller chose a chain.
export HARN_SECRET_PROVIDERS="${HARN_SECRET_PROVIDERS:-env}"
# Harn compilation and VM setup can exceed Rust's 2 MiB spawned-thread
# default. The production CLI uses 16 MiB; tests mirror it unless overridden.
export RUST_MIN_STACK="${RUST_MIN_STACK:-16777216}"

# One private scratch root per invocation: a fresh durable session store, so
# fixed fixture session IDs cannot resume another process's transcript, and an
# empty XDG config home, so the developer's ~/.config/harn (and every other
# per-user tool config) is invisible, as it is on a CI runner. XDG rather than
# a Harn-owned variable: build tooling may run an older installed `harn`
# inside this environment, and a Harn release rejects `HARN_*` names newer
# than itself.
scratch_root="$(mktemp -d "${TMPDIR:-/tmp}/harn-test-env.XXXXXX")"
trap 'rm -rf -- "$scratch_root"' EXIT
mkdir -p "$scratch_root/sessions" "$scratch_root/config"

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
if (( per_test_state == 0 )); then
  export HARN_SESSION_STORE_ROOT="$scratch_root/sessions"
fi
export XDG_CONFIG_HOME="$scratch_root/config"
# Windows resolves user configuration through APPDATA rather than XDG.
export APPDATA="$scratch_root/config"

"$@" &
child_pid=$!
if wait "$child_pid"; then
  status=0
else
  status=$?
fi
child_pid=""
exit "$status"
