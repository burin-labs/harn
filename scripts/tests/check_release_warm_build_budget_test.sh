#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT="${ROOT}/scripts/check_release_warm_build_budget.sh"
POLICY="${ROOT}/.github/release-warm-build-budget.json"
JQ="${ROOT}/.github/release-warm-build-budget.jq"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

jq -e -f "$JQ" "$POLICY" >/dev/null || fail "policy failed closed jq contract"

out="$("$SCRIPT" --target x86_64-apple-darwin --duration 100 --mode warm --enforce warn)"
grep -q 'status=ok' <<<"$out" || fail "expected ok for fast warm build: $out"

out="$("$SCRIPT" --target x86_64-apple-darwin --duration 1948 --mode warm --enforce warn)"
grep -q 'status=warn' <<<"$out" || fail "expected warn at baseline: $out"

if "$SCRIPT" --target x86_64-apple-darwin --duration 3000 --mode warm --enforce fail >/tmp/warm-budget-over.txt 2>&1; then
  fail "expected hard failure over budget"
fi
grep -q 'status=over_budget' /tmp/warm-budget-over.txt || fail "missing over_budget status"

out="$("$SCRIPT" --target x86_64-apple-darwin --duration 3000 --mode benchmark --enforce fail)"
grep -q 'status=over_budget' <<<"$out" || fail "benchmark should still report status"
grep -q 'informational only' <<<"$out" || fail "benchmark must not enforce"

echo "check_release_warm_build_budget_test.sh: ok"
