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
cargo nextest run --locked --workspace --profile ci --no-run \
  -E 'not test(test_linux_process_sandbox_catches_ten_process_escapes)'
# Match rust-check-inputs' security archive compile shape (harn-vm filter).
cargo nextest run --locked -p harn-vm --profile ci --no-run \
  -E 'test(test_linux_process_sandbox_catches_ten_process_escapes)'

# Workspace-crate cache canary touch for #5003 hosted wall-time sampling.
