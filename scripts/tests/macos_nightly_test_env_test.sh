#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="$ROOT_DIR/.github/workflows/macos-nightly.yml"

if ! grep -Fq \
  'scripts/ci/run_rust_test_lane.sh cargo nextest run --workspace --profile ci' \
  "$workflow"; then
  echo "macOS nightly must use the canonical Rust test environment" >&2
  exit 1
fi

echo "macos_nightly_test_env_test: ok"
