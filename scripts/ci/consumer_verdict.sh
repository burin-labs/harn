#!/usr/bin/env bash
# CI status's downstream-consumer decision. Only a measured pass from a
# successful owning workflow passes. A measured failure always blocks.
# Job/step hashes cannot establish equivalent failed cases or execution phases;
# they remain diagnostic metadata, never evidence of an inherited failure.
#
# Inputs are the CONSUMER_* variables CI status sets. Exit 1 blocks.
set -uo pipefail

summary() {
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then echo "- $1" >> "$GITHUB_STEP_SUMMARY"; fi
}

unmeasured() {
  local message="consumer check UNMEASURED: $1"
  summary "$message"
  echo "::error::$message"
  exit 1
}

block() {
  summary "consumer check BLOCKS: $1"
  echo "::error::$1"
  exit 1
}

echo "Downstream consumer: result=${CONSUMER_RESULT:-} verdict=${CONSUMER_VERDICT:-none} (expected=${CONSUMER_EXPECTED:-})"
case "${CONSUMER_EXPECTED:-}" in
  true | false) ;;
  *) unmeasured "consumer_scope_unmeasured" ;;
esac
if [ "${CONSUMER_EXPECTED:-}" != "true" ]; then
  case "${CONSUMER_RESULT:-}" in
    success | skipped) exit 0 ;;
    *) block "Downstream consumer check ran without being expected and did not pass." ;;
  esac
fi

case "${CONSUMER_VERDICT:-}" in
  pass)
    if [ "${CONSUMER_RESULT:-}" != success ]; then
      block "Downstream consumer pass has no successful owning workflow (${CONSUMER_RESULT:-unmeasured})."
    fi
    exit 0 ;;
  fail)
    block "Downstream consumer run ${CONSUMER_RUN:-none} measured a failure. Baseline job/step matches are incomparable without source-bound case evidence and cannot excuse it." ;;
  *) unmeasured "consumer_unmeasured (${CONSUMER_REASONS:-})" ;;
esac
