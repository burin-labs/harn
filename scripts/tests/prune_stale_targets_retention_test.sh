#!/usr/bin/env bash
set -euo pipefail

# Retention falsifiers for the shared per-worktree Cargo target cache.
#
# Each case pins one decision the GC has to get right, and each was observed
# failing before the retention pass existed:
#
#   1. A worktree directory that was deleted while its git administrative
#      record survived. `git worktree list` still reports such a record, so a
#      keep-set built from that listing protects the orphan forever. This is
#      the case that let a cache entry outlive the thing it caches.
#   2. A live worktree's entry is kept and named in the report.
#   3. An entry a live process is building into is kept even though its mtime
#      is ancient, because a build paused between steps reads as idle.
#   4. The liveness probe must not match its own process ancestry. A GC
#      launched from a command line that names an entry must still collect it.
#   5. An empty cache root reports a scanned count, so "found no orphans" is
#      distinguishable from "scanned nothing".

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
gc="$repo_root/scripts/prune_stale_targets.sh"
tmp_root=$(mktemp -d)
live_pids=()

cleanup() {
  local pid
  if [ "${#live_pids[@]}" -gt 0 ]; then
    for pid in "${live_pids[@]}"; do
      kill "$pid" 2>/dev/null || true
    done
  fi
  rm -rf "$tmp_root"
}
trap cleanup EXIT

fail() {
  echo "prune_stale_targets_retention_test: $1" >&2
  shift
  local log
  for log in "$@"; do
    [[ -f "$log" ]] && { echo "--- $log ---" >&2; cat "$log" >&2; }
  done
  exit 1
}

storage="$tmp_root/storage"
repos="$tmp_root/repos"
targets="$storage/harn-target"
mkdir -p "$targets" "$repos"

# A live worktree, and a worktree deleted from disk whose git record survives.
git -C "$repos" init -q live
git -C "$repos/live" config user.email gc@example.invalid
git -C "$repos/live" config user.name gc
git -C "$repos/live" commit -q --allow-empty -m seed
git -C "$repos/live" worktree add -q "$repos/deleted" -b deleted
rm -rf "$repos/deleted"

# Entry names mirror dev_setup.sh::derive_target_dir: <parent>-<leaf>.
repos_leaf="$(basename "$repos")"
live_entry="$targets/${repos_leaf}-live"
ghost_entry="$targets/${repos_leaf}-deleted"
lock_entry="$targets/${repos_leaf}-lockheld"
cmdline_entry="$targets/${repos_leaf}-cmdline"
mkdir -p "$live_entry" "$ghost_entry" "$lock_entry/debug" "$cmdline_entry"

# Every entry is far older than any age bound, so only worktree existence and
# process liveness may decide.
touch -t 202001010000 "$live_entry" "$ghost_entry" "$lock_entry" "$cmdline_entry"

# A build holding cargo's advisory lock, and a build that names its target dir
# on its command line. Both are real processes; neither touches the tree again.
# Each stand-in is exactly one process, so the cleanup trap can end it. A
# wrapper shell would leave its `sleep` child holding this script's output
# pipe, which stalls the whole suite rather than failing it.
: > "$lock_entry/debug/.cargo-lock"
sleep 600 9<"$lock_entry/debug/.cargo-lock" >/dev/null 2>&1 &
live_pids+=("$!")
bash -c 'exec -a "cargo build --target-dir $0" sleep 600' "$cmdline_entry" \
  >/dev/null 2>&1 &
live_pids+=("$!")
touch -t 202001010000 "$lock_entry" "$cmdline_entry" "$lock_entry/debug"

run_gc() {
  HARN_DEV_SETUP_STORAGE_ROOT="$storage" \
    HARN_TARGET_GC_ROOTS="$repos" \
    HARN_TARGET_GC_MIN_AGE_SECS=1 \
    bash "$gc" "$@"
}

dry="$tmp_root/dry.txt"
run_gc --dry-run >"$dry" 2>&1

grep -Fq "would remove orphan: ${repos_leaf}-deleted" "$dry" \
  || fail "a cache entry whose worktree directory is gone was not collected" "$dry"
grep -Fq "${repos_leaf}-live" "$dry" \
  || fail "the kept live-worktree entry was not reported by name" "$dry"
grep -Fq "${repos_leaf}-lockheld" "$dry" \
  || fail "the kept lock-held entry was not reported by name" "$dry"
grep -Fq "${repos_leaf}-cmdline" "$dry" \
  || fail "the kept in-flight-build entry was not reported by name" "$dry"
grep -Eq 'scanned=4' "$dry" \
  || fail "the summary did not report how many entries it scanned" "$dry"
for keeper in "$live_entry" "$lock_entry" "$cmdline_entry"; do
  grep -Fq "would remove orphan: $(basename "$keeper")" "$dry" \
    && fail "an entry that must be kept was selected for removal: $keeper" "$dry"
done

# The probe must exclude its own process ancestry. This launcher's command line
# names the orphan, which is exactly the shape that manufactures a live owner
# for the entry being scanned.
selftest="$tmp_root/selftest.txt"
HARN_DEV_SETUP_STORAGE_ROOT="$storage" \
  HARN_TARGET_GC_ROOTS="$repos" \
  HARN_TARGET_GC_MIN_AGE_SECS=1 \
  bash -c 'exec bash "$0" --dry-run' "$gc" "$ghost_entry" >"$selftest" 2>&1
grep -Fq "would remove orphan: ${repos_leaf}-deleted" "$selftest" \
  || fail "the liveness probe matched its own process ancestry" "$selftest"

# Real run: read back every decision.
run="$tmp_root/run.txt"
run_gc >"$run" 2>&1
[[ -e "$ghost_entry" ]] && fail "the orphan survived a real run" "$run"
for keeper in "$live_entry" "$lock_entry" "$cmdline_entry"; do
  [[ -d "$keeper" ]] || fail "a kept entry was removed: $keeper" "$run"
done

# Negative control: an empty cache root must report a scanned count of zero,
# not exit quietly.
empty_storage="$tmp_root/empty/harn-target"
mkdir -p "$empty_storage"
empty="$tmp_root/empty.txt"
HARN_DEV_SETUP_STORAGE_ROOT="$tmp_root/empty" \
  HARN_TARGET_GC_ROOTS="$repos" \
  bash "$gc" --dry-run >"$empty" 2>&1
grep -Fq 'scanned=0' "$empty" \
  || fail "an empty cache root did not report a zero scanned count" "$empty"

# Print what the pass actually decided. A green run that shows only "ok"
# cannot be told apart from one that scanned nothing.
echo "--- retention report (dry run) ---"
cat "$dry"
echo "--- empty cache root (negative control) ---"
cat "$empty"
echo "prune_stale_targets_retention_test: ok"
