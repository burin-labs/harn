#!/usr/bin/env bash
# Warm the shared Linux package-audit Cargo graph on refs/heads/main.
#
# Exact-SHA merge-group proof reuse skips package-audit on main push, and
# rust-cache save-if only persists from refs/heads/main. This script is the
# post-merge writer that keeps the next merge_group package-audit restore from
# compiling cold. It matches the package-audit job's verify invocation and must
# not inject mold/RUSTFLAGS the audit lane does not use (#5003).
set -euo pipefail

# Match package-audit: keep the combined Harn/AOT-generator Cargo invocation.
export HARN_BIN=""

./scripts/verify_crate_packages.sh
