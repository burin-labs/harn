#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
script="$repo_root/scripts/ci/run_rust_test_lane.sh"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

summary="$tmpdir/summary.md"
log="$tmpdir/output.log"

# shellcheck disable=SC2016 # child bash expands CARGO_BUILD_JOBS
GITHUB_STEP_SUMMARY="$summary" "$script" bash -c 'test "$CARGO_BUILD_JOBS" = "2"' \
  >"$log" 2>&1
grep -q 'CARGO_BUILD_JOBS=2' "$summary"
grep -q 'Rust test resources before' "$summary"
grep -q 'Rust test resources after' "$summary"
grep -q '::group::Rust test resources before' "$log"

custom_summary="$tmpdir/custom-summary.md"
custom_log="$tmpdir/custom-output.log"
# shellcheck disable=SC2016 # child bash expands CARGO_BUILD_JOBS
CARGO_BUILD_JOBS=3 GITHUB_STEP_SUMMARY="$custom_summary" "$script" \
  bash -c 'test "$CARGO_BUILD_JOBS" = "3"' >"$custom_log" 2>&1
grep -q 'CARGO_BUILD_JOBS=3' "$custom_summary"

set +e
"$script" bash -c 'exit 23' >/dev/null 2>&1
status=$?
set -e
if [[ "$status" -ne 23 ]]; then
  echo "expected failing command status 23, got $status" >&2
  exit 1
fi
