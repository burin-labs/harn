#!/usr/bin/env bash
set -euo pipefail

# The target GC owns two roots under one storage root: `harn-target`, whose
# entries are named `<parent>-<leaf>` after the worktree that configured them,
# and `release-gate-target`, whose entries are named after the release root
# alone (#6212). Release worktrees are ephemeral, so without a GC every finished
# release would strand a multi-gigabyte Cargo cache somewhere the OS no longer
# reaps.

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

storage="$tmp_root/storage"
repos="$tmp_root/repos"
mkdir -p \
  "$storage/harn-target/live-root" \
  "$storage/harn-target/repos-live-root" \
  "$storage/release-gate-target/live-root" \
  "$storage/release-gate-target/orphan-root" \
  "$repos/live-root"
git -C "$repos/live-root" init -q

# Everything is old enough to prune, so only the keep-set decides.
touch -t 202001010000 \
  "$storage/harn-target/live-root" \
  "$storage/harn-target/repos-live-root" \
  "$storage/release-gate-target/live-root" \
  "$storage/release-gate-target/orphan-root"

run_gc() {
  HARN_DEV_SETUP_STORAGE_ROOT="$storage" \
    HARN_TARGET_GC_ROOTS="$repos" \
    HARN_TARGET_GC_MIN_AGE_SECS=1 \
    "$repo_root/scripts/prune_stale_targets.sh" "$@"
}

output="$tmp_root/dry-run.txt"
run_gc --dry-run > "$output" 2>&1

# A release-gate cache whose release root is gone is exactly what accumulates.
if ! grep -Fq 'would remove orphan: orphan-root' "$output"; then
  echo "orphaned release-gate target was not collected" >&2
  cat "$output" >&2
  exit 1
fi

# `harn-target/live-root` is named like a release root, not like a setup target
# (`repos-live-root`). Sharing one keep-set between the two roots would protect
# it here and leak setup targets forever.
if ! grep -Fq 'would remove orphan: live-root' "$output"; then
  echo "a setup target named like a release root must not inherit its keep-set" >&2
  cat "$output" >&2
  exit 1
fi

if ! grep -Fq 'release-gate-target' "$output"; then
  echo "summary did not report the release-gate root it walked" >&2
  cat "$output" >&2
  exit 1
fi

# Nothing is removed in a dry run.
for survivor in \
  "$storage/release-gate-target/orphan-root" \
  "$storage/release-gate-target/live-root" \
  "$storage/harn-target/repos-live-root"; do
  if [[ ! -d "$survivor" ]]; then
    echo "dry run removed $survivor" >&2
    exit 1
  fi
done

run_gc > "$tmp_root/run.txt" 2>&1

if [[ -d "$storage/release-gate-target/orphan-root" ]]; then
  echo "orphaned release-gate target survived a real run" >&2
  cat "$tmp_root/run.txt" >&2
  exit 1
fi
if [[ ! -d "$storage/release-gate-target/live-root" ]]; then
  echo "release-gate cache for a live worktree must be kept" >&2
  cat "$tmp_root/run.txt" >&2
  exit 1
fi
if [[ ! -d "$storage/harn-target/repos-live-root" ]]; then
  echo "setup target for a live worktree must be kept" >&2
  cat "$tmp_root/run.txt" >&2
  exit 1
fi

echo "prune_stale_targets_test: ok"
