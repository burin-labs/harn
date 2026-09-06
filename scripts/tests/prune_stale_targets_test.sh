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
minimum_bash=$(command -v bash)
if [[ -x /bin/bash ]]; then
  minimum_bash=/bin/bash
fi

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
    "$minimum_bash" "$repo_root/scripts/prune_stale_targets.sh" "$@"
}

assert_single_root_modes() {
  local setup_only="$tmp_root/setup-only"
  local release_only="$tmp_root/release-only"
  local empty="$tmp_root/empty"

  mkdir -p \
    "$setup_only/harn-target/orphan-setup" \
    "$release_only/release-gate-target/orphan-release" \
    "$empty"
  touch -t 202001010000 \
    "$setup_only/harn-target/orphan-setup" \
    "$release_only/release-gate-target/orphan-release"

  HARN_DEV_SETUP_STORAGE_ROOT="$setup_only" \
    HARN_TARGET_GC_ROOTS="$repos" \
    HARN_TARGET_GC_MIN_AGE_SECS=1 \
    "$minimum_bash" "$repo_root/scripts/prune_stale_targets.sh" --dry-run \
      >"$tmp_root/setup-only.txt" 2>&1
  if ! grep -Fq 'would remove orphan: orphan-setup' "$tmp_root/setup-only.txt"; then
    echo "setup-only target pruning did not complete" >&2
    cat "$tmp_root/setup-only.txt" >&2
    exit 1
  fi

  HARN_DEV_SETUP_STORAGE_ROOT="$release_only" \
    HARN_TARGET_GC_ROOTS="$repos" \
    HARN_TARGET_GC_MIN_AGE_SECS=1 \
    "$minimum_bash" "$repo_root/scripts/prune_stale_targets.sh" --dry-run \
      >"$tmp_root/release-only.txt" 2>&1
  if ! grep -Fq 'would remove orphan: orphan-release' "$tmp_root/release-only.txt"; then
    echo "release-only target pruning did not complete" >&2
    cat "$tmp_root/release-only.txt" >&2
    exit 1
  fi

  HARN_DEV_SETUP_STORAGE_ROOT="$empty" \
    HARN_TARGET_GC_ROOTS="$repos" \
    "$minimum_bash" "$repo_root/scripts/prune_stale_targets.sh" --dry-run \
      >"$tmp_root/no-roots.txt" 2>&1
  if ! grep -Fq 'nothing to prune' "$tmp_root/no-roots.txt"; then
    echo "empty target pruning did not return its no-op result" >&2
    cat "$tmp_root/no-roots.txt" >&2
    exit 1
  fi
}

assert_single_root_modes

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

# --remove-entry: rank and age are what the caller overrides, so the entry the
# automatic rules were protecting is the one this must take. The live entry is
# the negative control; without it a flag that removed everything named would
# pass this test just as happily.
if [[ ! -d "$storage/harn-target/repos-live-root" ]]; then
  echo "expected the live entry to still exist before the named-removal case" >&2
  exit 1
fi
mkdir -p "$storage/harn-target/repos-finished-lane"
printf 'artifact\n' > "$storage/harn-target/repos-finished-lane/artifact"

run_gc --dry-run --remove-entry repos-finished-lane > "$tmp_root/named-dry.txt" 2>&1
if [[ ! -d "$storage/harn-target/repos-finished-lane" ]]; then
  echo "dry run removed a named entry" >&2
  cat "$tmp_root/named-dry.txt" >&2
  exit 1
fi
grep -Fq "would remove named by caller: repos-finished-lane" "$tmp_root/named-dry.txt" || {
  echo "dry run did not report the named entry" >&2
  cat "$tmp_root/named-dry.txt" >&2
  exit 1
}

# A live process outranks the caller: the caller can be wrong about a lane being
# finished, but not about a compiler holding the tree open. The probe matches a
# process whose command line names the entry, so this control holds a real one
# rather than leaning on rank, which is exactly what --remove-entry overrides.
busy="$storage/harn-target/repos-busy-lane"
mkdir -p "$busy"
printf 'artifact\n' > "$busy/artifact"
# Hold the entry the way Cargo does, through its lock file. The probe's other
# arm reads process arguments, which `ps` truncates for the long temporary
# paths this test builds, so a cmdline-based control would hang waiting for a
# match that can never appear. The lock arm is lsof-based and path-length
# independent.
: > "$busy/.cargo-lock"
exec 9<> "$busy/.cargo-lock"
sleep 60 <&9 &
busy_pid=$!
trap 'kill "$busy_pid" 2>/dev/null || true; exec 9>&-' EXIT
until lsof -t -- "$busy/.cargo-lock" 2>/dev/null | grep -q .; do sleep 0.2; done

run_gc --remove-entry repos-finished-lane --remove-entry repos-busy-lane > "$tmp_root/named.txt" 2>&1
if [[ -d "$storage/harn-target/repos-finished-lane" ]]; then
  echo "named entry survived a real run" >&2
  cat "$tmp_root/named.txt" >&2
  exit 1
fi
if [[ ! -d "$busy" ]]; then
  echo "a named entry with a live process must still be kept" >&2
  cat "$tmp_root/named.txt" >&2
  exit 1
fi
kill "$busy_pid" 2>/dev/null || true

# A name matching nothing must say so; otherwise a typo reads as success.
run_gc --remove-entry no-such-entry > "$tmp_root/named-miss.txt" 2>&1
grep -Fq "matched no entry in any managed root: no-such-entry" "$tmp_root/named-miss.txt" || {
  echo "an unmatched --remove-entry name was not reported" >&2
  cat "$tmp_root/named-miss.txt" >&2
  exit 1
}

# A name that could address a path outside the root is refused at parse time.
if run_gc --remove-entry ../escape > "$tmp_root/named-bad.txt" 2>&1; then
  echo "a path-bearing --remove-entry name was accepted" >&2
  exit 1
fi
grep -Fq "invalid --remove-entry name" "$tmp_root/named-bad.txt" || {
  echo "the refusal did not name the reason" >&2
  cat "$tmp_root/named-bad.txt" >&2
  exit 1
}

# --- size ceiling ---------------------------------------------------------
# Rank and age keep an entry however large the root grows, which is the whole
# defect: a root can sit at any size with every rule reporting a pass. These
# cases pin the ceiling AND the two things it must never override.
size_root="$tmp_root/size"
size_storage="$size_root/storage"
size_repos="$size_root/repos"
mkdir -p "$size_storage/harn-target" "$size_repos"

# shellcheck disable=SC2119,SC2120  # called both with and without arguments
run_size_gc() {
  HARN_DEV_SETUP_STORAGE_ROOT="$size_storage" \
    HARN_TARGET_GC_ROOTS="$size_repos" \
    HARN_TARGET_GC_MIN_AGE_SECS=1 \
    "$minimum_bash" "$repo_root/scripts/prune_stale_targets.sh" "$@"
}

# Three warm entries, each backed by a live worktree so rank and age keep them
# all. 2 MiB apiece, so any ceiling below 6 MiB must bite.
for n in old mid new; do
  mkdir -p "$size_repos/$n"
  git -C "$size_repos/$n" init -q 2>/dev/null || true
  mkdir -p "$size_storage/harn-target/repos-$n"
  dd if=/dev/zero of="$size_storage/harn-target/repos-$n/blob" bs=1024 count=2048 \
    >/dev/null 2>&1
done
# Distinct activity stamps so "coldest first" has something to order by. The
# stamp comes from the ENTRY DIRECTORY, not its contents, and writing the blob
# above bumped it, so these have to land after the writes.
touch -t 202001010000 "$size_storage/harn-target/repos-old"
touch -t 202001020000 "$size_storage/harn-target/repos-mid"

echo "the ceiling is off by default: an oversized root is left alone"
run_size_gc > "$tmp_root/size-off.txt" 2>&1
for n in old mid new; do
  [[ -d "$size_storage/harn-target/repos-$n" ]] || {
    echo "default run evicted repos-$n with no ceiling set" >&2
    cat "$tmp_root/size-off.txt" >&2
    exit 1
  }
done
grep -Fq "size ceiling" "$tmp_root/size-off.txt" && {
  echo "the size pass ran with no ceiling configured" >&2
  exit 1
}

echo "a ceiling below the root size evicts coldest first until it fits"
# 5 MiB: one 2 MiB entry must go, and it must be the oldest one.
HARN_TARGET_GC_MAX_BYTES=5242880 run_size_gc > "$tmp_root/size-on.txt" 2>&1
[[ ! -d "$size_storage/harn-target/repos-old" ]] || {
  echo "the coldest entry survived the ceiling" >&2
  cat "$tmp_root/size-on.txt" >&2
  exit 1
}
for n in mid new; do
  [[ -d "$size_storage/harn-target/repos-$n" ]] || {
    echo "the ceiling evicted repos-$n past the point where the root fit" >&2
    cat "$tmp_root/size-on.txt" >&2
    exit 1
  }
done
grep -Fq "over the size ceiling" "$tmp_root/size-on.txt" || {
  echo "the eviction did not name the size ceiling as its reason" >&2
  exit 1
}

echo "a ceiling above the root size changes nothing"
HARN_TARGET_GC_MAX_BYTES=1073741824 run_size_gc > "$tmp_root/size-big.txt" 2>&1
for n in mid new; do
  [[ -d "$size_storage/harn-target/repos-$n" ]] || {
    echo "a generous ceiling still evicted repos-$n" >&2
    cat "$tmp_root/size-big.txt" >&2
    exit 1
  }
done
grep -Fq "over the size ceiling" "$tmp_root/size-big.txt" && {
  echo "a generous ceiling still reported an eviction" >&2
  exit 1
}

echo "a live process outranks the ceiling"
: > "$size_storage/harn-target/repos-mid/.cargo-lock"
exec 8<> "$size_storage/harn-target/repos-mid/.cargo-lock"
sleep 60 <&8 &
size_busy_pid=$!
trap 'kill "$size_busy_pid" 2>/dev/null || true; exec 8>&-' EXIT
until lsof -t -- "$size_storage/harn-target/repos-mid/.cargo-lock" 2>/dev/null | grep -q .; do
  sleep 0.2
done
# 1 MiB is below even a single entry, so without the liveness guard the pass
# would try to evict everything it can reach.
HARN_TARGET_GC_MAX_BYTES=1048576 run_size_gc > "$tmp_root/size-live.txt" 2>&1
[[ -d "$size_storage/harn-target/repos-mid" ]] || {
  echo "the ceiling evicted an entry held by a live process" >&2
  cat "$tmp_root/size-live.txt" >&2
  exit 1
}
kill "$size_busy_pid" 2>/dev/null || true

echo "a rejected ceiling value fails loudly instead of reading as unlimited"
if HARN_TARGET_GC_MAX_BYTES=not-a-number run_size_gc > "$tmp_root/size-bad.txt" 2>&1; then
  echo "a non-numeric ceiling was accepted" >&2
  exit 1
fi
grep -Fq "HARN_TARGET_GC_MAX_BYTES must be a non-negative integer" "$tmp_root/size-bad.txt" || {
  echo "the refusal did not name the reason" >&2
  cat "$tmp_root/size-bad.txt" >&2
  exit 1
}

echo "prune_stale_targets_test: ok"
