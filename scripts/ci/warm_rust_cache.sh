#!/usr/bin/env bash
# Warm the shared Linux workspace-tests Cargo graph on refs/heads/main.
#
# Exact-SHA merge-group proof reuse skips the compile lanes on main push, and
# rust-cache save-if only persists from refs/heads/main. This script is the
# post-merge writer that keeps the next merge_group restore from compiling
# cold. It matches the compile shape used by rust-check-inputs and the
# colocated workspace-tests leg without re-running the suite.
set -euo pipefail

cargo build --locked --bin harn
cargo nextest run --locked --workspace --profile ci --no-run \
  -E 'not test(test_linux_process_sandbox_catches_ten_process_escapes)'
