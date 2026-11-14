#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
threads="${1:-}"
if [[ ! "$threads" =~ ^[1-9][0-9]*$ ]]; then
  echo "usage: $0 <positive-thread-count>" >&2
  exit 2
fi
reporter="${HARN_BIN:?prepare HARN_BIN before starting thread-count qualification}"
if [[ ! -x "$reporter" ]]; then
  echo "thread_parity_receipt reason=reporter-unavailable" >&2
  exit 2
fi

tmpdir="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/harn-thread-parity.XXXXXX")"
inventory="$tmpdir/inventory.json"
events="$tmpdir/events.jsonl"
receipt="$tmpdir/receipt.json"
child_pid=""
inventory_complete=0
runner_status=0
: >"$events"
if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
  printf 'evidence_dir=%s\n' "$tmpdir" >>"$GITHUB_OUTPUT"
fi

finish() {
  local status=$? receipt_status=0
  trap - EXIT INT TERM
  if ((inventory_complete)); then
    "$reporter" run --standalone --no-sandbox \
      "$repo_root/scripts/ci/summarize_nextest_receipt.harn" -- \
      --inventory "$inventory" --events "$events" --runner-status "$runner_status" \
      --threads "$threads" --receipt "$receipt" || receipt_status=$?
  else
    echo "thread_parity_receipt reason=inventory-error threads=$threads inventory_status=$status" >&2
  fi
  echo "thread_parity_evidence path=$tmpdir" >&2
  if ((status == 0 && receipt_status != 0)); then
    status=$receipt_status
  fi
  exit "$status"
}

interrupt() {
  local status=$1
  trap - INT TERM
  runner_status=$status
  if [[ -n "$child_pid" ]]; then
    # Async Bash children inherit INT ignored. TERM reaches their cleanup trap.
    kill -s TERM "$child_pid" 2>/dev/null || true
    wait "$child_pid" 2>/dev/null || true
  fi
  exit "$status"
}
trap finish EXIT
trap 'interrupt 130' INT
trap 'interrupt 143' TERM

cargo-nextest nextest list --locked --workspace --profile ci --message-format json \
  >"$inventory" &
child_pid=$!
inventory_status=0
wait "$child_pid" || inventory_status=$?
child_pid=""
if ((inventory_status != 0)); then
  exit "$inventory_status"
fi
inventory_complete=1

RUST_TEST_STDOUT_PATH="$events" \
NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1 \
  "$repo_root/scripts/ci/run_rust_test_lane.sh" \
  cargo-nextest nextest run --locked --workspace --profile ci \
  --test-threads="$threads" --status-level=pass --message-format libtest-json-plus \
  --message-format-version 0.1 &
child_pid=$!
wait "$child_pid" || runner_status=$?
child_pid=""
exit "$runner_status"
