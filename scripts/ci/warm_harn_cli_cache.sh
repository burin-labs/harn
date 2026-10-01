#!/usr/bin/env bash
# Warm the shared Linux harn-cli Cargo graph on refs/heads/main.
#
# `rust-cli` builds the shared Harn CLI in its own job and restores its own
# cache key, so the merge-queue CLI build reads this graph rather than the
# workspace-tests one. It matches `scripts/ci/rust_artifact.sh build-cli`.
#
# Pair with cache-workspace-crates=true on the writer (#5003).
set -euo pipefail

cargo build --locked --bin harn
