#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="$ROOT_DIR/.github/workflows/macos-nightly.yml"
# An exact-source dispatch runs on the organization's own Apple Silicon
# runners; the paid M4 class requires the explicit repository opt-in on top.
# An unset or malformed variable therefore stays on owned capacity instead of
# silently restoring paid capacity, and any non-dispatch event stays hosted.
dispatch_runner="runs-on: \${{ github.event_name == 'workflow_dispatch' && (vars.HARN_CI_ENABLE_BLACKSMITH_MACOS == 'true' && 'blacksmith-12vcpu-macos-15' || 'macos-arm64') || 'macos-latest' }}"
# The dispatch budgets belong to warm builds: 30 minutes on the paid class,
# 45 on the owned runners whose first build after a toolchain change is cold.
# A cold pull-request or scheduled run needs the nightly's budget: this lane's
# p90 is 47 minutes, and a timeout reads as a red lane rather than a slow one.
dispatch_timeout="timeout-minutes: \${{ github.event_name == 'workflow_dispatch' && (vars.HARN_CI_ENABLE_BLACKSMITH_MACOS == 'true' && 30 || 45) || 75 }}"

if ! grep -Fq "$dispatch_runner" "$workflow"; then
  echo "macOS workspace tests must require an explicit opt-in for the paid M4 runner" >&2
  exit 1
fi

if ! grep -Fq "$dispatch_timeout" "$workflow"; then
  echo "macOS workspace tests must bound dispatch and non-dispatch hangs independently" >&2
  exit 1
fi

# A pull-request run must never reach the paid class or the short budget. Both
# expressions name the dispatch event positively, so any event that is not a
# dispatch falls to the hosted runner and the generous budget by construction.
if grep -Fq "github.event_name != 'pull_request' && 'blacksmith" "$workflow"; then
  echo "macOS workspace tests must not route pull requests to the paid M4 class" >&2
  exit 1
fi

if ! grep -Fq \
  'scripts/ci/run_rust_test_lane.sh cargo nextest run --locked --workspace --profile ci' \
  "$workflow"; then
  echo "macOS workspace tests must use the canonical Rust test environment" >&2
  exit 1
fi

if ! grep -Fq \
  'uses: actions/setup-go@b7ad1dad31e06c5925ef5d2fc7ad053ef454303e # v7.0.0' \
  "$workflow"; then
  echo "macOS workspace tests must provision the pinned formatter used by generated Go contracts" >&2
  exit 1
fi

performance_id_line="$(grep -Fn 'id: release-test-case-performance' "$workflow" | cut -d: -f1)"
performance_command_line="$(grep -Fn 'run: make check-test-case-performance' "$workflow" | cut -d: -f1)"
performance_binary_line="$(grep -Fn "HARN_BIN: \${{ env.CARGO_TARGET_DIR || './target' }}/debug/harn" "$workflow" | cut -d: -f1)"
performance_profile_line="$(grep -Fn 'HARN_TEST_CASE_PERFORMANCE_PROFILE: macos_hosted_arm64' "$workflow" | cut -d: -f1)"
nextest_line="$(grep -Fn 'scripts/ci/run_rust_test_lane.sh cargo nextest run --locked --workspace --profile ci' "$workflow" | cut -d: -f1)"

if [[ -z "$performance_id_line" || -z "$performance_command_line" || -z "$performance_binary_line" || -z "$performance_profile_line" ]]; then
  echo "macOS workspace tests must own the exact release test-case performance proof" >&2
  exit 1
fi

if [[ "$performance_id_line" -le "$nextest_line" || "$performance_command_line" -le "$nextest_line" ]]; then
  echo "macOS workspace tests must measure release performance after workspace tests settle" >&2
  exit 1
fi

# A self-hosted run must build into the host's persistent target and must not
# let the hosted cache restore replace it; both are keyed on the same context.
if ! grep -Fq 'echo "CARGO_TARGET_DIR=${target}" >> "$GITHUB_ENV"' "$workflow"; then
  echo "macOS workspace tests must build into the persistent target on self-hosted runners" >&2
  exit 1
fi
if ! grep -Fq "if: \${{ runner.environment != 'self-hosted' }}" "$workflow"; then
  echo "macOS workspace tests must not restore the hosted cache over a persistent target" >&2
  exit 1
fi

echo "macos_nightly_test_env_test: ok"
