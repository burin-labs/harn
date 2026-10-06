#!/usr/bin/env bash
# Warm the shared Linux workspace-tests Cargo graph on refs/heads/main.
#
# Exact-SHA merge-group proof reuse skips the compile lanes on main push, and
# rust-cache save-if only persists from refs/heads/main. This script is the
# post-merge writer that keeps the next merge_group restore from compiling
# cold. It matches the compile shape used by rust-check-inputs and the
# colocated workspace-tests leg without re-running the suite.
#
# Pair with cache-workspace-crates=true on the writer: Swatinem otherwise
# strips workspace artifacts before save and merge_group still rebuilds every
# harn-* crate after an exact key hit (#5003).
set -euo pipefail

cargo build --locked --bin harn
host_bound_filter="$(scripts/ci/host_bound_rust_test_filter.sh)"
cargo-nextest nextest run --locked --workspace --profile ci --no-run \
  -E "not (${host_bound_filter})"
# Match rust-check-inputs' exact GitHub-owned security archive compile shape.
cargo-nextest nextest run --locked --workspace --profile ci --no-run \
  -E '(package(harn-vm) and binary(harn_vm)) or (package(harn-hostlib) and binary(harn_hostlib))'

# Workspace-crate cache canary touch for #5003 hosted wall-time sampling.

# Drop the linked test executables before the post step saves this target.
# Every consumer relinks them anyway: a pull request that touches harn-vm or
# anything below it (nearly all of them) rebuilds all 22 dependent workspace
# crates and relinks every test binary, so the copies only cost space in the
# 10 GB repository cache, where this family was the largest entry and its
# size pushed other merge-gate families out (#9430). Compiled libraries and
# build-script outputs stay, which is what a restore actually reuses.
target_dir="$(cargo metadata --format-version 1 --no-deps | jq -er '.target_directory')"
before_kib="$(du -sk "$target_dir" | cut -f1)"
find "$target_dir/debug/deps" -maxdepth 1 -type f -perm -u+x ! -name '*.*' -delete
after_kib="$(du -sk "$target_dir" | cut -f1)"
echo "workspace cache: dropped linked test executables, $((before_kib / 1024)) MiB -> $((after_kib / 1024)) MiB"
