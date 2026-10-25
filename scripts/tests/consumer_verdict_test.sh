#!/usr/bin/env bash
# CI status may excuse a measured downstream consumer fail only when the base
# rehearsal measured the same failure; every other base outcome must block.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
verdict="$root/scripts/ci/consumer_verdict.sh"

# The aggregate must actually run this decision, or the cases below prove nothing.
grep -q 'bash scripts/ci/consumer_verdict.sh || exit 1' "$root/.github/workflows/ci.yml"

HEAD_A=$(printf 'a%.0s' {1..40})
HEAD_B=$(printf 'b%.0s' {1..40})
TESTS_FAILED=1111111111111111111111111111111111111111111111111111111111111111
SETUP_FAILED=2222222222222222222222222222222222222222222222222222222222222222

# expect <exit code> <output fragment> <VAR=value>...
expect() {
  local want=$1 fragment=$2 out status
  shift 2
  set +e
  out=$(env -i PATH="$PATH" \
    CONSUMER_EXPECTED=true CONSUMER_RESULT=failure/skipped CONSUMER_VERDICT=fail \
    CONSUMER_RUN=11 CONSUMER_BASE_RUN=22 CONSUMER_BASE="$HEAD_B" \
    CONSUMER_IDENTITY="$HEAD_A/jobs" CONSUMER_BASE_IDENTITY="$HEAD_A/jobs" \
    CONSUMER_FAILURES="$TESTS_FAILED" CONSUMER_BASE_CLI_RESULT=success \
    CONSUMER_BASE_RESULT=failure/skipped \
    CONSUMER_REASONS='first=none retry=none' CONSUMER_BASE_REASONS='first=none retry=none' \
    "$@" bash "$verdict" 2>&1)
  status=$?
  set -e
  if [[ "$status" != "$want" ]] || ! grep -qF -- "$fragment" <<< "$out"; then
    printf 'consumer verdict case failed (%s): want exit %s with "%s", got exit %s:\n%s\n' "$*" "$want" "$fragment" "$status" "$out" >&2
    exit 1
  fi
}

# The base CLI never installed, so the base measured nothing: still red.
expect 1 'base rehearsal measured nothing' CONSUMER_BASE_VERDICT= CONSUMER_BASE_FAILURES=
# The base failed at setup while this change failed in its tests: not the same failure.
expect 1 'fails in different job steps' CONSUMER_BASE_VERDICT=fail CONSUMER_BASE_FAILURES="$SETUP_FAILED"
# Neither side named where it failed.
expect 1 'fails in different job steps' CONSUMER_BASE_VERDICT=fail CONSUMER_FAILURES= CONSUMER_BASE_FAILURES=
expect 1 'passes at base' CONSUMER_BASE_VERDICT=pass CONSUMER_BASE_FAILURES=
expect 1 'rehearsed a different consumer' CONSUMER_BASE_VERDICT=fail CONSUMER_BASE_FAILURES="$TESTS_FAILED" \
  CONSUMER_BASE_IDENTITY="$HEAD_B/jobs"
expect 1 'rehearsed a different consumer' CONSUMER_BASE_VERDICT=fail CONSUMER_BASE_FAILURES="$TESTS_FAILED" \
  CONSUMER_BASE_IDENTITY="$HEAD_A/other"
# Only the same measured failure at the same consumer is inherited.
expect 0 'red is inherited' CONSUMER_BASE_VERDICT=fail CONSUMER_BASE_FAILURES="$TESTS_FAILED"

expect 0 'verdict=pass' CONSUMER_VERDICT=pass
expect 1 '::error::consumer check UNMEASURED' CONSUMER_VERDICT=
expect 1 '::error::consumer check UNMEASURED' CONSUMER_VERDICT= CONSUMER_REASONS='first=no_verdict_before_deadline retry=observation_window_elapsed'
expect 1 '::error::consumer check UNMEASURED' CONSUMER_VERDICT= CONSUMER_RESULT=cancelled/cancelled
expect 0 'expected=false' CONSUMER_EXPECTED=false CONSUMER_RESULT=skipped/skipped
expect 1 'ran without being expected' CONSUMER_EXPECTED=false CONSUMER_RESULT=failure/skipped
echo "consumer verdict: OK"
