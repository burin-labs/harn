#!/usr/bin/env bash
#
# Advisory diagnostic: report Cargo FEATURE drift that a lockfile diff cannot see.
#
# Cargo unifies features across the whole dependency graph. When any crate in
# the tree turns on a feature of a shared dependency, that feature is enabled
# for EVERY consumer of it, including this workspace's own code. So a dependency
# bump can change the behavior of a crate whose version never moved — and
# `git diff Cargo.lock` shows nothing, because no version changed.
#
# This is not hypothetical. It landed in #5127:
#
#   jsonschema 0.46 -> 0.48 added `serde_json/float_roundtrip`, which unified
#   onto harn-cli's own serde_json usage and changed float PARSING repo-wide.
#   `bench_profile_json_includes_iterations_stats_and_rollup` asserts a
#   serialized p95 equals 13.8; the computed value is 13.799999999999999 (one
#   ULP below). Without float_roundtrip serde_json's parser rounds it back to
#   13.8 and the assertion passes; with exact round-trip parsing it does not.
#   The `Rust test` gate went red with serde_json's version unchanged at
#   1.0.150 in both lockfiles.
#
# What it reports: packages present at the SAME version in both the baseline
# and the working tree whose resolved FEATURE SET differs. Added and removed
# features are listed separately. Version changes are deliberately NOT
# reported — `git diff Cargo.lock` already covers those, and mixing the two
# buries the signal this script exists to surface.
#
# Every flag in the snapshot below is load-bearing:
#   --target all      platform-gated dependencies vanish from the default
#                     host-only resolution (this is how the Linux-only
#                     wayland/xcb subtree stays visible on macOS).
#   -e normal,build   build-dependencies carry features too, and they affect
#                     what codegen runs at build time.
#   sed/awk/sort -u   strip cargo's tree-drawing prefixes and `(*)` dedup
#                     markers, then dedup: a package appears once per path
#                     through the graph, so the raw output is ~900 lines for
#                     a handful of distinct packages.
#
# The snapshot also strips the absolute source path that `{p}` prints for
# path-dependencies (`harn-vm v0.10.23 (/abs/path/crates/harn-vm)`). The
# baseline is resolved in a temporary worktree under a different absolute
# path, so leaving those in makes every workspace member look like it drifted.
#
# Usage:
#   scripts/check_feature_drift.sh [baseline-ref]     # default: origin/main
#
# Exit status is ALWAYS 0. This is advisory: feature drift is frequently
# intentional, and a reviewer has to judge each case. It is meant to be run
# when preparing or reviewing a dependency PR, so the drift is stated rather
# than discovered later as an unexplained test failure.
#
# The baseline is resolved in a throwaway `git worktree`, so the working tree
# and its Cargo.lock are never modified.

set -euo pipefail

baseline_ref=${1:-origin/main}

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

if ! git rev-parse --verify --quiet "$baseline_ref" >/dev/null; then
  echo "error: baseline ref '$baseline_ref' not found (try: git fetch origin)" >&2
  exit 2
fi

snapshot() {
  # One line per (package, version, feature-set), deduplicated.
  cargo tree --workspace --target all -e normal,build -f '{p} {f}' 2>/dev/null |
    sed 's/^[^a-zA-Z]*//; s/ (\*)$//; s| ([^)]*/[^)]*)||g' |
    awk 'NF' |
    sort -u
}

work_dir=$(mktemp -d)
baseline_tree="$work_dir/baseline"
cleanup() {
  git worktree remove --force "$baseline_tree" >/dev/null 2>&1 || true
  rm -rf "$work_dir"
}
trap cleanup EXIT

echo "==> resolving current tree"
snapshot > "$work_dir/current.txt"

echo "==> resolving baseline ($baseline_ref)"
git worktree add --detach "$baseline_tree" "$baseline_ref" >/dev/null 2>&1
(cd "$baseline_tree" && snapshot) > "$work_dir/baseline.txt"

echo "==> comparing feature sets"
echo

"$repo_root/scripts/harn_bin.sh" run "$repo_root/scripts/feature_drift.harn" -- \
  --baseline "$work_dir/baseline.txt" \
  --current "$work_dir/current.txt" \
  --baseline-ref "$baseline_ref"
