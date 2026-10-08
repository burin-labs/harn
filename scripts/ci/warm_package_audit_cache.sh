#!/usr/bin/env bash
# Warm the shared Linux package-audit Cargo graph on refs/heads/main.
#
# The scheduled writer refreshes main's package-audit graph between pushes.
# It must not inject mold/RUSTFLAGS the audit job does not use (#5003).
set -euo pipefail

# Match package-audit: keep the combined Harn/AOT-generator Cargo invocation.
export HARN_BIN=""

./scripts/verify_crate_packages.sh
