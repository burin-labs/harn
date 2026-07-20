#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
script="$repo_root/scripts/ci/run_rust_test_lane.sh"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

summary="$tmpdir/summary.md"
log="$tmpdir/output.log"

# Archive-only consumers do not compile; the lane must stay bounded by an
# execution-only timeout. The expected value is owned by
# scripts/check_ci_cache_policy.harn (EXECUTION_ONLY_TIMEOUT_MINUTES), which the
# check-ci-cache-policy gate enforces against ci.yml — only assert presence here
# so the two guards cannot drift on the literal.
rust_test_block="$(sed -n '/^  rust-test:/,/^  rust-security:/p' "$repo_root/.github/workflows/ci.yml")"
grep -qE '^    timeout-minutes: [0-9]+$' <<<"$rust_test_block"

GITHUB_STEP_SUMMARY="$summary" "$script" true >"$log" 2>&1
grep -q 'Rust test resources before' "$summary"
grep -q 'Rust test resources after' "$summary"
grep -q 'Rust test execution' "$summary"
grep -q 'Exit status: 0' "$summary"
grep -q '::group::Rust test resources before' "$log"
grep -q 'rust_test_execution_seconds=' "$log"

custom_summary="$tmpdir/custom-summary.md"
custom_log="$tmpdir/custom-output.log"
GITHUB_STEP_SUMMARY="$custom_summary" "$script" true >"$custom_log" 2>&1
grep -q 'Rust test execution' "$custom_summary"

set +e
"$script" bash -c 'exit 23' >/dev/null 2>&1
status=$?
set -e
if [[ "$status" -ne 23 ]]; then
  echo "expected failing command status 23, got $status" >&2
  exit 1
fi
