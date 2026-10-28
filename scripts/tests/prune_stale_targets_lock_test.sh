#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
gc="$repo_root/scripts/prune_stale_targets.sh"
fixture="$(mktemp -d)"
trap 'rm -rf -- "$fixture"' EXIT
storage="$fixture/storage"
lock="$storage/prune-stale-targets.lock"
mkdir -p "$lock" "$fixture/repos"

# The test's own process is a known-live holder, regardless of lock age.
printf '%s %s\n' "$$" 1 > "$lock/owner"
output="$(HARN_DEV_SETUP_STORAGE_ROOT="$storage" HARN_TARGET_GC_ROOTS="$fixture/repos" HARN_TARGET_GC_LOCK_STALE_SECS=1 bash "$gc" --dry-run 2>&1)"
if [[ "$output" != *"status=skipped reason=another-run-active holder_pid=$$ "* ]]; then
  printf '%s\n' "FAIL: a live old holder did not retain the run lock" "$output" >&2
  exit 1
fi
[[ "$(cat "$lock/owner")" == "$$ 1" ]] || { echo 'FAIL: live lock was modified' >&2; exit 1; }

rm "$lock/owner"
rmdir "$lock"
mkdir "$lock"
true &
dead=$!
wait "$dead"
if kill -0 "$dead" 2>/dev/null; then
  echo 'FAIL: exited test PID was reused' >&2
  exit 1
fi
printf '%s %s\n' "$dead" 1 > "$lock/owner"
output="$(HARN_DEV_SETUP_STORAGE_ROOT="$storage" HARN_TARGET_GC_ROOTS="$fixture/repos" HARN_TARGET_GC_LOCK_STALE_SECS=1 bash "$gc" --dry-run 2>&1)"
[[ "$output" == *"replacing stale run lock (holder_pid=$dead "* ]] || { echo 'FAIL: dead holder was not recovered' >&2; exit 1; }
[[ ! -e "$lock" ]] || { echo 'FAIL: recovered lock remains' >&2; exit 1; }

mkdir "$lock"
output="$(HARN_DEV_SETUP_STORAGE_ROOT="$storage" HARN_TARGET_GC_ROOTS="$fixture/repos" HARN_TARGET_GC_LOCK_STALE_SECS=3600 bash "$gc" --dry-run 2>&1)"
[[ "$output" == *'status=skipped reason=another-run-active '* ]] || { echo 'FAIL: new unrecorded lock was replaced' >&2; exit 1; }
touch -t 201801010000 "$lock"
output="$(HARN_DEV_SETUP_STORAGE_ROOT="$storage" HARN_TARGET_GC_ROOTS="$fixture/repos" HARN_TARGET_GC_LOCK_STALE_SECS=3600 bash "$gc" --dry-run 2>&1)"
[[ "$output" == *'replacing stale run lock (holder_pid=unknown '* ]] || { echo 'FAIL: old unrecorded lock was not recovered' >&2; exit 1; }

mkdir "$lock" "$fixture/bin"
printf '%s %s\n' "$dead" 1 > "$lock/owner"
printf '#!/bin/sh\nexit 2\n' > "$fixture/bin/ps"
chmod +x "$fixture/bin/ps"
output="$(PATH="$fixture/bin:$PATH" HARN_DEV_SETUP_STORAGE_ROOT="$storage" HARN_TARGET_GC_ROOTS="$fixture/repos" HARN_TARGET_GC_LOCK_STALE_SECS=1 bash "$gc" --dry-run 2>&1)"
[[ "$output" == *'status=skipped reason=another-run-active '* ]] || { echo 'FAIL: unknown process state was treated as dead' >&2; exit 1; }
[[ "$(cat "$lock/owner")" == "$dead 1" ]] || { echo 'FAIL: unknown owner lock was modified' >&2; exit 1; }
echo 'prune_stale_targets_lock_test: ok'
