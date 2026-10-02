#!/usr/bin/env bash
# Warm the shared Linux harn-cli Cargo graph on refs/heads/main.
#
# `rust-cli` builds the shared Harn CLI in its own job and restores its own
# cache key, so the merge-queue CLI build reads this graph rather than the
# workspace-tests one. It matches `scripts/ci/rust_artifact.sh build-cli`,
# including its `ci-cli` profile (SHARED_CLI_PROFILE there): warming the dev
# profile would leave the producer recompiling the whole CLI after a hit.
#
# Pair with cache-workspace-crates=true on the writer (#5003).
set -euo pipefail

cargo build --locked --profile ci-cli --bin harn
