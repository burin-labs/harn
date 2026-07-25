#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
script="$repo_root/scripts/ci/run_rust_test_lane.sh"
tmpdir="$(mktemp -d)"

# Every assertion below is a bare `grep -q` against a captured log or summary,
# so under `set -e` a failure aborts with no indication of what the file
# actually held. Print them before the temp dir goes away.
cleanup() {
  local status=$?
  if [[ "$status" -ne 0 ]]; then
    for captured in "$tmpdir"/*.log "$tmpdir"/*.md; do
      [[ -f "$captured" ]] || continue
      echo "--- $(basename "$captured") ---" >&2
      cat "$captured" >&2
    done
  fi
  rm -rf "$tmpdir"
  exit "$status"
}
trap cleanup EXIT

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

# Test workers mirror the production CLI stack by default, but callers can
# deliberately exercise another stack size.
"$script" bash -c 'test "$RUST_MIN_STACK" = 16777216' >/dev/null 2>&1
RUST_MIN_STACK=4194304 "$script" \
  bash -c 'test "$RUST_MIN_STACK" = 4194304' >/dev/null 2>&1

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
