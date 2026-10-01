#!/usr/bin/env bash
# Run one Harn test target as concurrent shard processes.
#
# `harn test --parallel` schedules user tests on threads inside one process.
# The agent-loop suite does not scale that way: on 2026-10-01, four in-process
# workers took 171s where one serial process took 117s, and two cases that pass
# in under two seconds alone hit the 30s execute ceiling. Four shard processes
# finished the same 677 cases in 35s. Process shards partition the cases with
# `--shard-index`/`--shard-total`, so every case still runs exactly once.
#
# Usage: scripts/run_harn_test_shards.sh [--slice K/N] TARGET [harn test args...]
#   --slice K/N     run slice K of N runner slices. The target is cut into
#                   N * HARN_TEST_JOBS shards and this slice runs its own
#                   HARN_TEST_JOBS of them, so N runners together cover every
#                   case once. Default 1/1.
#   HARN_BIN        Harn executable (default: scripts/harn_bin.sh --print)
#   HARN_TEST_JOBS  shard process count (default: available processors, max 4)
set -euo pipefail

usage() {
  echo "usage: $0 [--slice K/N] TARGET [harn test args...]" >&2
  exit 2
}

slice_index=1
slice_total=1
if [ "${1:-}" = "--slice" ]; then
  [ "$#" -ge 2 ] || usage
  if [[ ! "$2" =~ ^([1-9][0-9]*)/([1-9][0-9]*)$ ]] \
    || [ "${BASH_REMATCH[1]}" -gt "${BASH_REMATCH[2]}" ]; then
    echo "error: invalid --slice '$2' (expected K/N with 1 <= K <= N)" >&2
    exit 2
  fi
  slice_index="${BASH_REMATCH[1]}"
  slice_total="${BASH_REMATCH[2]}"
  shift 2
fi
[ "$#" -ge 1 ] || usage
target="$1"
shift

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
harn_bin="${HARN_BIN:-$("$script_dir/harn_bin.sh" --print)}"

default_jobs="$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)"
[ "$default_jobs" -gt 4 ] && default_jobs=4
jobs="${HARN_TEST_JOBS:-$default_jobs}"
case "$jobs" in
  ''|*[!0-9]*|0) echo "error: HARN_TEST_JOBS must be a positive integer, got: $jobs" >&2; exit 2 ;;
esac

log_dir="$(mktemp -d "${TMPDIR:-/tmp}/harn-test-shards.XXXXXX")"
pids=()
cleanup() {
  local pid
  for pid in "${pids[@]}"; do
    kill "$pid" 2>/dev/null || true
  done
  rm -rf "$log_dir"
}
trap cleanup EXIT

shard_total=$((slice_total * jobs))
first_shard=$(((slice_index - 1) * jobs + 1))
started="$(date +%s)"
for index in $(seq 1 "$jobs"); do
  shard_index=$((first_shard + index - 1))
  # Each shard gets its own isolated test environment, including a fresh
  # session store, so fixed fixture session IDs cannot collide across shards.
  "$script_dir/harn_test_env.sh" "$harn_bin" test "$target" \
    --shard-index "$shard_index" --shard-total "$shard_total" "$@" \
    >"$log_dir/shard-$index.log" 2>&1 &
  pids+=("$!")
done

status=0
failed=()
for index in $(seq 1 "$jobs"); do
  if wait "${pids[$((index - 1))]}"; then
    :
  else
    shard_status=$?
    status=$shard_status
    failed+=("$((first_shard + index - 1))")
  fi
  echo "--- $target shard $((first_shard + index - 1))/$shard_total ---"
  cat "$log_dir/shard-$index.log"
done
pids=()

elapsed=$(( $(date +%s) - started ))
if [ "$status" -ne 0 ]; then
  echo "FAIL: $target shard(s) ${failed[*]} of slice $slice_index/$slice_total failed (${elapsed}s)" >&2
  exit "$status"
fi
echo "ok: $target slice $slice_index/$slice_total in $jobs shard processes (${elapsed}s)"
