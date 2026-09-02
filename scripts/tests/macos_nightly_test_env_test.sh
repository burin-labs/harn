#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="$ROOT_DIR/.github/workflows/macos-nightly.yml"
dispatch_runner="runs-on: \${{ github.event_name == 'schedule' && 'macos-latest' || 'blacksmith-12vcpu-macos-15' }}"
dispatch_timeout="timeout-minutes: \${{ github.event_name == 'schedule' && 75 || 30 }}"

if ! grep -Fq "$dispatch_runner" "$workflow"; then
  echo "macOS nightly must reserve the proven M4 runner for exact-source dispatches" >&2
  exit 1
fi

if ! grep -Fq "$dispatch_timeout" "$workflow"; then
  echo "macOS nightly must bound scheduled and exact-source hangs independently" >&2
  exit 1
fi

if ! grep -Fq \
  'scripts/ci/run_rust_test_lane.sh cargo nextest run --locked --workspace --profile ci' \
  "$workflow"; then
  echo "macOS nightly must use the canonical Rust test environment" >&2
  exit 1
fi

if ! grep -Fq \
  'uses: actions/setup-go@b7ad1dad31e06c5925ef5d2fc7ad053ef454303e # v7.0.0' \
  "$workflow"; then
  echo "macOS nightly must provision the pinned formatter used by generated Go contracts" >&2
  exit 1
fi

performance_id_line="$(grep -Fn 'id: release-test-case-performance' "$workflow" | cut -d: -f1)"
performance_command_line="$(grep -Fn 'run: make check-test-case-performance' "$workflow" | cut -d: -f1)"
performance_binary_line="$(grep -Fn 'HARN_BIN: ./target/debug/harn' "$workflow" | cut -d: -f1)"
performance_profile_line="$(grep -Fn 'HARN_TEST_CASE_PERFORMANCE_PROFILE: macos_hosted_arm64' "$workflow" | cut -d: -f1)"
nextest_line="$(grep -Fn 'scripts/ci/run_rust_test_lane.sh cargo nextest run --locked --workspace --profile ci' "$workflow" | cut -d: -f1)"

if [[ -z "$performance_id_line" || -z "$performance_command_line" || -z "$performance_binary_line" || -z "$performance_profile_line" ]]; then
  echo "macOS nightly must own the exact release test-case performance proof" >&2
  exit 1
fi

if [[ "$performance_id_line" -le "$nextest_line" || "$performance_command_line" -le "$nextest_line" ]]; then
  echo "macOS nightly must measure release performance after workspace tests settle" >&2
  exit 1
fi

echo "macos_nightly_test_env_test: ok"
