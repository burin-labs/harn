#!/usr/bin/env bash
set -euo pipefail

# Falsifiers for the host maintenance policy (`--host-maintenance`), the one the
# scheduled and disk-pressure sweeps run.
#
# The setup defaults keep the 10 most recent warm trees whatever their age. On
# a build host with 10 or fewer entries that cap protects every entry, so the
# idle bound never fires: a 460 GiB host held 133 GB of trees nobody had built
# in up to 42 hours and filled its disk while the daily sweep reported
# `kept=5 removed=0`.
#
#   1. Under the setup defaults, a small host keeps every stale entry. This is
#      the observed failure, pinned so case 2 cannot pass vacuously.
#   2. Under the host policy, the same host retires the stale entries past the
#      3 most recent, while recently built entries survive.
#   3. Negative control: an entry a live build holds is kept by the same run,
#      however old it is.
#   4. The policy reports its bounds and where the ceiling came from: the
#      filesystem size, the fallback when that size is unreadable, or an
#      explicit override.

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
  echo "prune_stale_targets_host_policy_test: $1" >&2
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

git -C "$repos" init -b main -q main
git -C "$repos/main" config user.email gc@example.invalid
git -C "$repos/main" config user.name gc
git -C "$repos/main" commit -q --allow-empty -m seed

# Six entries, all with live worktrees, so only rank, idle age, and liveness can
# decide. Entry names mirror dev_setup.sh::derive_target_dir: <parent>-<leaf>.
repos_leaf="$(basename "$repos")"
for lane in fresh-a fresh-b stale-a stale-b stale-c held; do
  git -C "$repos/main" worktree add -q "$repos/$lane" -b "$lane"
  mkdir -p "$targets/${repos_leaf}-$lane/debug"
done
touch "$targets/${repos_leaf}-fresh-a/debug" "$targets/${repos_leaf}-fresh-b/debug"
touch -t 202003010000 "$targets/${repos_leaf}-stale-a/debug"
touch -t 202002010000 "$targets/${repos_leaf}-stale-b/debug"
touch -t 202001010000 "$targets/${repos_leaf}-stale-c/debug"
held="$targets/${repos_leaf}-held"
: > "$held/debug/.cargo-lock"
sleep 600 9<"$held/debug/.cargo-lock" >/dev/null 2>&1 &
live_pids+=("$!")
touch -t 201901010000 "$held/debug"
for lane in fresh-a fresh-b stale-a stale-b stale-c held; do
  touch -t 201901010000 "$targets/${repos_leaf}-$lane"
done

run_gc() {
  HARN_DEV_SETUP_STORAGE_ROOT="$storage" \
    HARN_TARGET_GC_ROOTS="$repos" \
    HARN_TARGET_GC_MIN_AGE_SECS=1 \
    bash "$gc" "$@"
}

# 1. The setup defaults: a host this small keeps everything.
setup_defaults="$tmp_root/setup-defaults.txt"
run_gc --dry-run >"$setup_defaults" 2>&1
grep -Fq "would remove" "$setup_defaults" \
  && fail "the setup defaults retired an entry; case 2 no longer shows the policy is what fires" "$setup_defaults"
grep -Fq "keep (within the 10 most recent): ${repos_leaf}-stale-c" "$setup_defaults" \
  || fail "the setup defaults did not keep the oldest stale entry on rank" "$setup_defaults"

# 2 and 3. The host policy, as a real run with every decision read back.
host_run="$tmp_root/host-run.txt"
run_gc --host-maintenance >"$host_run" 2>&1 \
  || fail "the host-policy run failed" "$host_run"
for gone in stale-b stale-c; do
  grep -Fq "removing cold cache: ${repos_leaf}-$gone" "$host_run" \
    || fail "a stale entry past the 3 most recent was not retired: $gone" "$host_run"
  [[ -e "$targets/${repos_leaf}-$gone" ]] \
    && fail "a retired entry is still on disk: $gone" "$host_run"
done
for keep in fresh-a fresh-b stale-a; do
  [[ -d "$targets/${repos_leaf}-$keep" ]] \
    || fail "an entry inside the 3 most recent was removed: $keep" "$host_run"
done
grep -Fq "keep (live process" "$host_run" \
  || fail "the live-owned entry was not reported as kept for its process" "$host_run"
[[ -d "$held" ]] || fail "an entry a live build holds was removed" "$host_run"
grep -Eq 'status=complete .*removed=2 ' "$host_run" \
  || fail "the summary did not report exactly two removals" "$host_run"

# 4. The bounds are reported, and the ceiling's source is named.
grep -Eq '^harn-target GC host policy: keep_recent=3 max_idle_secs=259200 max_bytes=[1-9][0-9]* max_bytes_source=disk$' "$host_run" \
  || fail "the host policy did not report a disk-scaled ceiling" "$host_run"
fs_kib="$(df -Pk "$targets" | awk 'NR == 2 { print $2 }')"
grep -Fq "max_bytes=$((fs_kib * 1024 / 8)) " "$host_run" \
  || fail "the ceiling is not an eighth of the cache's filesystem" "$host_run"

mkdir -p "$tmp_root/no-df"
printf '#!/bin/sh\nexit 1\n' > "$tmp_root/no-df/df"
chmod +x "$tmp_root/no-df/df"
fallback="$tmp_root/fallback.txt"
PATH="$tmp_root/no-df:$PATH" run_gc --dry-run --host-maintenance >"$fallback" 2>&1
grep -Fq "max_bytes=68719476736 max_bytes_source=fallback" "$fallback" \
  || fail "an unreadable filesystem size did not fall back to 64 GiB" "$fallback"

override="$tmp_root/override.txt"
HARN_TARGET_GC_MAX_BYTES=1073741824 HARN_TARGET_GC_KEEP_RECENT=5 \
  run_gc --dry-run --host-maintenance >"$override" 2>&1
grep -Fq "keep_recent=5 max_idle_secs=259200 max_bytes=1073741824 max_bytes_source=env" "$override" \
  || fail "explicit settings did not override the host policy" "$override"

echo "--- setup defaults on a small host (the observed failure) ---"
cat "$setup_defaults"
echo "--- host maintenance policy ---"
cat "$host_run"
echo "prune_stale_targets_host_policy_test: ok"
