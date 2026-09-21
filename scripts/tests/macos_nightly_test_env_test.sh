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

# THE FACTS, NOT ONE CONCATENATED LITERAL. This used to grep for the whole
# command as a single string, so it asserted the flags' exact spelling AND
# their exact order AND that nothing sat between them. Adding `--exclude
# harn-wasm` in the middle of that run line therefore reddened the default
# branch while changing nothing this test exists to protect, and the lane the
# test lives in runs only on a push to the default branch, so the pull request
# that added the flag could not have been told.
#
# Each fact is now checked on its own, on the one line that carries them, so a
# flag inserted between two of them passes and a missing one still fails.
nextest_command="$(grep -F 'run_rust_test_lane.sh cargo nextest run' "$workflow" || true)"
if [[ -z "$nextest_command" ]]; then
  echo "macOS workspace tests must run through scripts/ci/run_rust_test_lane.sh" >&2
  exit 1
fi
if [[ "$(grep -Fc 'run_rust_test_lane.sh cargo nextest run' "$workflow")" != "1" ]]; then
  # Two such lines and the checks below could each be satisfied by a different
  # one, which would report a canonical environment nothing actually runs.
  echo "macOS workspace tests must have exactly one workspace nextest invocation" >&2
  exit 1
fi
for required in '--locked' '--workspace' '--profile ci'; do
  if ! grep -Fq -- "$required" <<<"$nextest_command"; then
    echo "macOS workspace tests must use the canonical Rust test environment" >&2
    echo "  the workspace nextest step is missing ${required}" >&2
    echo "  the line reads: ${nextest_command}" >&2
    exit 1
  fi
done

if ! grep -Fq \
  'uses: actions/setup-go@b7ad1dad31e06c5925ef5d2fc7ad053ef454303e # v7.0.0' \
  "$workflow"; then
  echo "macOS workspace tests must provision the pinned formatter used by generated Go contracts" >&2
  exit 1
fi

performance_id_line="$(grep -Fn 'id: release-test-case-performance' "$workflow" | cut -d: -f1)"
performance_command_line="$(grep -Fn 'make check-test-case-performance' "$workflow" | cut -d: -f1)"
performance_binary_line="$(grep -Fn 'export HARN_BIN="${CARGO_TARGET_DIR:-./target}/debug/harn"' "$workflow" | cut -d: -f1)"
performance_profile_line="$(grep -Fn "HARN_TEST_CASE_PERFORMANCE_PROFILE: \${{ runner.environment == 'self-hosted' && 'macos_owned_arm64' || 'macos_hosted_arm64' }}" "$workflow" | cut -d: -f1)"
nextest_line="$(grep -Fn 'run_rust_test_lane.sh cargo nextest run' "$workflow" | cut -d: -f1)"

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
# The persistent target is keyed on the runner: two runners on one host sharing
# a target would let one job's rebuild replace the binary another job still runs.
if ! grep -Fq 'target="${HOME}/harn-ci/target/macos-workspace-${RUNNER_NAME' "$workflow"; then
  echo "macOS workspace tests must key the persistent target on the runner, not the host" >&2
  exit 1
fi
# Owned hardware is judged against its own measured baseline profile.
if ! grep -Fq 'macos_owned_arm64' "$ROOT_DIR/bench/test-case-performance/baselines.toml"; then
  echo "the owned Apple Silicon performance profile must exist in the baseline file" >&2
  exit 1
fi
if ! grep -Fq "if: \${{ runner.environment != 'self-hosted' }}" "$workflow"; then
  echo "macOS workspace tests must not restore the hosted cache over a persistent target" >&2
  exit 1
fi

echo "macos_nightly_test_env_test: ok"
