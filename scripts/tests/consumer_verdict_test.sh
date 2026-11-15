#!/usr/bin/env bash
# CI status requires a measured pass from a successful owning workflow.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
verdict="$root/scripts/ci/consumer_verdict.sh"

# The aggregate must actually run this decision, or the cases below prove nothing.
grep -q 'bash scripts/ci/consumer_verdict.sh || exit 1' "$root/.github/workflows/ci.yml"

HEAD_A=$(printf 'a%.0s' {1..40})
HEAD_B=$(printf 'b%.0s' {1..40})
TESTS_FAILED=1111111111111111111111111111111111111111111111111111111111111111
measured=0

# expect <exit code> <output fragment> <VAR=value>...
expect() {
  local want=$1 fragment=$2 out status
  shift 2
  set +e
  out=$(env -i PATH="$PATH" \
    CONSUMER_EXPECTED=true CONSUMER_RESULT=failure CONSUMER_VERDICT=fail \
    CONSUMER_RUN=11 CONSUMER_REASONS='first=none retry=none' \
    "$@" bash "$verdict" 2>&1)
  status=$?
  set -e
  if [[ "$status" != "$want" ]] || ! grep -qF -- "$fragment" <<< "$out"; then
    printf 'consumer verdict case failed (%s): want exit %s with "%s", got exit %s:\n%s\n' "$*" "$want" "$fragment" "$status" "$out" >&2
    exit 1
  fi
  measured=$((measured + 1))
}

# Legacy coarse identities cannot authorize an inherited-failure exemption.
expect 1 'are incomparable' CONSUMER_BASE_VERDICT=fail CONSUMER_BASE_FAILURES="$TESTS_FAILED" \
  CONSUMER_BASE="$HEAD_B" CONSUMER_IDENTITY="$HEAD_A/jobs" CONSUMER_BASE_IDENTITY="$HEAD_A/jobs" \
  CONSUMER_FAILURES="$TESTS_FAILED"
expect 1 'measured a failure' CONSUMER_BASE_VERDICT=pass
expect 1 'measured a failure' CONSUMER_BASE_VERDICT=

expect 0 'verdict=pass' CONSUMER_VERDICT=pass CONSUMER_RESULT=success
expect 1 'no successful owning workflow' CONSUMER_VERDICT=pass CONSUMER_RESULT=failure
expect 1 'no successful owning workflow' CONSUMER_VERDICT=pass CONSUMER_RESULT=cancelled
expect 1 'no successful owning workflow' CONSUMER_VERDICT=pass CONSUMER_RESULT=skipped
expect 1 'no successful owning workflow' CONSUMER_VERDICT=pass CONSUMER_RESULT=
expect 1 'no successful owning workflow' CONSUMER_VERDICT=pass CONSUMER_RESULT=failure/success
expect 1 '::error::consumer check UNMEASURED' CONSUMER_VERDICT=
expect 1 '::error::consumer check UNMEASURED' CONSUMER_VERDICT= CONSUMER_REASONS='first=no_verdict_before_deadline retry=observation_window_elapsed'
expect 1 '::error::consumer check UNMEASURED' CONSUMER_VERDICT= CONSUMER_RESULT=cancelled
expect 1 'consumer_scope_unmeasured' CONSUMER_EXPECTED= CONSUMER_RESULT=skipped
expect 0 'expected=false' CONSUMER_EXPECTED=false CONSUMER_RESULT=skipped CONSUMER_VERDICT=
expect 1 'ran without being expected' CONSUMER_EXPECTED=false CONSUMER_RESULT=failure CONSUMER_VERDICT=
expect 1 'ran without being expected' CONSUMER_EXPECTED=false CONSUMER_RESULT=success CONSUMER_VERDICT=
expect 1 'measured a failure' CONSUMER_EXPECTED=false CONSUMER_RESULT=success CONSUMER_VERDICT=fail
expect 1 'measured a failure' CONSUMER_EXPECTED=false CONSUMER_RESULT=skipped CONSUMER_VERDICT=fail
expect 1 'no successful owning workflow' CONSUMER_EXPECTED=false CONSUMER_RESULT=skipped CONSUMER_VERDICT=pass
expect 1 'no successful owning workflow' CONSUMER_EXPECTED=false CONSUMER_RESULT=failure CONSUMER_VERDICT=pass
expect 1 'no successful owning workflow' CONSUMER_EXPECTED=false CONSUMER_RESULT= CONSUMER_VERDICT=pass
expect 0 'verdict=pass' CONSUMER_EXPECTED=false CONSUMER_RESULT=success CONSUMER_VERDICT=pass
echo "consumer verdict: measured=$measured failed=0 pending=0"
