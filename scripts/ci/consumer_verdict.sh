#!/usr/bin/env bash
# CI status's downstream-consumer decision. A measured pass passes. A measured
# fail is excused as inherited only when the base rehearsal also measured a
# fail on the same consumer commit and job set, failing in exactly the same
# job steps. Any other base outcome (passed, unmeasured, failed elsewhere, or
# a different consumer) leaves the pull request's measured fail blocking. An
# unmeasured pull request outcome is a named warning until
# CONSUMER_UNMEASURED_BLOCKS flips it to blocking.
#
# Inputs are the CONSUMER_* variables CI status sets. Exit 1 blocks.
set -uo pipefail

summary() {
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then echo "- $1" >> "$GITHUB_STEP_SUMMARY"; fi
}

unmeasured() {
  local message="consumer check UNMEASURED: $1"
  summary "$message"
  if [ "${CONSUMER_UNMEASURED_BLOCKS:-false}" = "true" ]; then
    echo "::error::$message"
    exit 1
  fi
  echo "::warning::$message"
  exit 0
}

block() {
  summary "consumer check BLOCKS: $1"
  echo "::error::$1"
  exit 1
}

echo "Downstream consumer: result=${CONSUMER_RESULT:-} verdict=${CONSUMER_VERDICT:-none} (expected=${CONSUMER_EXPECTED:-})"
if [ "${CONSUMER_EXPECTED:-}" != "true" ]; then
  case "${CONSUMER_RESULT%%/*}" in
    success | skipped) exit 0 ;;
    *) block "Downstream consumer check ran without being expected and did not pass." ;;
  esac
fi

case "${CONSUMER_VERDICT:-}" in
  pass) exit 0 ;;
  fail) ;;
  *) unmeasured "consumer_unmeasured (${CONSUMER_REASONS:-})" ;;
esac

base=${CONSUMER_BASE:-unknown}
run=${CONSUMER_RUN:-none}
base_run=${CONSUMER_BASE_RUN:-none}
echo "Downstream consumer at base $base: base CLI=${CONSUMER_BASE_CLI_RESULT:-} result=${CONSUMER_BASE_RESULT:-} verdict=${CONSUMER_BASE_VERDICT:-none}"
identity=${CONSUMER_IDENTITY:-/}
base_identity=${CONSUMER_BASE_IDENTITY:-/}
same_head=false
same_jobs=false
[ -n "${identity%%/*}" ] && [ "${identity%%/*}" = "${base_identity%%/*}" ] && same_head=true
[ -n "${identity#*/}" ] && [ "${identity#*/}" = "${base_identity#*/}" ] && same_jobs=true

case "${CONSUMER_BASE_VERDICT:-}" in
  pass | fail) ;;
  *) block "Downstream consumer run $run fails with this change and the base rehearsal measured nothing (${CONSUMER_BASE_REASONS:-}, base CLI=${CONSUMER_BASE_CLI_RESULT:-}), so the red is not shown to be inherited." ;;
esac
if [ "$same_head" != "true" ] || [ "$same_jobs" != "true" ]; then
  block "Downstream consumer run $run fails with this change and base run $base_run rehearsed a different consumer (same commit=$same_head, same jobs=$same_jobs), so the red is not shown to be inherited."
fi
if [ "$CONSUMER_BASE_VERDICT" = "pass" ]; then
  block "Downstream consumer run $run fails with this change and consumer run $base_run passes at base $base on the same consumer commit."
fi
failures=${CONSUMER_FAILURES:-}
if [ -z "$failures" ] || [ "$failures" != "${CONSUMER_BASE_FAILURES:-}" ]; then
  block "Downstream consumer run $run fails with this change and base run $base_run fails in different job steps, so the base did not measure the same failure."
fi
echo "::notice::Downstream consumer red is inherited: consumer run $run fails here and consumer run $base_run fails in the same job steps at base $base on the same consumer commit, so it does not block this pull request."
exit 0
