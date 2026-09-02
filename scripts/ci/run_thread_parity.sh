#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
threads="${1:-}"
if [[ ! "$threads" =~ ^[1-9][0-9]*$ ]]; then
  echo "usage: $0 <positive-thread-count>" >&2
  exit 2
fi

tmpdir="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/harn-thread-parity.XXXXXX")"
cleanup() {
  rm -rf "$tmpdir"
}
trap cleanup EXIT

inventory="$tmpdir/inventory.json"
events="$tmpdir/events.jsonl"
inventory_status=0
cargo nextest list --locked --workspace --profile ci --message-format json \
  >"$inventory" || inventory_status=$?
if [[ "$inventory_status" -ne 0 ]]; then
  echo "thread_parity_receipt reason=inventory-error threads=$threads inventory_status=$inventory_status" >&2
  exit "$inventory_status"
fi

runner_status=0
RUST_TEST_STDOUT_PATH="$events" \
NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1 \
  "$repo_root/scripts/ci/run_rust_test_lane.sh" \
  cargo nextest run --locked --workspace --profile ci \
  --test-threads="$threads" --message-format libtest-json-plus \
  --message-format-version 0.1 \
  || runner_status=$?

node "$repo_root/scripts/ci/summarize_nextest_receipt.mjs" \
  --inventory "$inventory" \
  --events "$events" \
  --runner-status "$runner_status" \
  --threads "$threads"
