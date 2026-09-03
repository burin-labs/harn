#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="$ROOT_DIR/.github/workflows/macos-nightly.yml"
# The paid M4 class is reserved for exact-source dispatches, which are the
# release's blocking proof. Everything else — the schedule, and pull requests
# once this lane runs on them — takes the hosted runner. Naming the dispatch
# event rather than excluding the schedule is what keeps a third event off the
# paid class by default instead of routing it there.
dispatch_runner="runs-on: \${{ github.event_name == 'workflow_dispatch' && 'blacksmith-12vcpu-macos-15' || 'macos-latest' }}"
# The 30 minute budget belongs to the dispatch, which restores a warm cache.
# A cold pull-request or scheduled run needs the nightly's budget: this lane's
# p90 is 47 minutes, and a timeout reads as a red lane rather than a slow one.
dispatch_timeout="timeout-minutes: \${{ github.event_name == 'workflow_dispatch' && 30 || 75 }}"

if ! grep -Fq "$dispatch_runner" "$workflow"; then
  echo "macOS nightly must reserve the proven M4 runner for exact-source dispatches" >&2
  exit 1
fi

if ! grep -Fq "$dispatch_timeout" "$workflow"; then
  echo "macOS nightly must bound dispatch and non-dispatch hangs independently" >&2
  exit 1
fi

# A pull-request run must never reach the paid class or the short budget. Both
# expressions name the dispatch event positively, so any event that is not a
# dispatch falls to the hosted runner and the generous budget by construction.
if grep -Fq "github.event_name != 'pull_request' && 'blacksmith" "$workflow"; then
  echo "macOS nightly must not route pull requests to the paid M4 class" >&2
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
