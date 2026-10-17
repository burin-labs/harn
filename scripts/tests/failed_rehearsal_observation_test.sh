#!/usr/bin/env bash
# Exercise the historical owning-step reader, including observations that
# superficially contain the right strings but must not authorize a cutover.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$root/scripts/lib/release_consumer_verdict.sh"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
source_sha=8743d2bd57df14eb1a022dec9cb842cbcbd7b568
producer=37222257040
child=37532968826

reset_logs() {
  cat > "$fixture/resolver" <<EOF
##[group]Run bash scripts/resolve-release-promotion-source.sh
env:
  CANDIDATE_RUN_ID: $producer
  EXPECTED_SOURCE_SHA: $source_sha
##[endgroup]
Post job cleanup.
Cleaning up orphan processes
EOF
  cat > "$fixture/consumer" <<EOF
##[group]Run CANARY_REPOSITORY="\$CANARY_OWNER/\$CANARY_NAME" \\
env:
  SOURCE_REVISION: $source_sha
  CANARY_WORKFLOW: harn-repin-rehearsal.yml
##[endgroup]
CONSUMER_CANARY dispatched run=$child ref=default started_at=1700000000
##[group]Run CANARY_REPOSITORY="\$CANARY_OWNER/\$CANARY_NAME" bash scripts/ci/consumer_canary.sh --observe
env:
  CANARY_RUN_ID: $child
  CANARY_STARTED_AT: 1700000000
  CANARY_WINDOW_SECONDS: 2700
  CANARY_DEADLINE_SECONDS: 7200
##[endgroup]
CONSUMER_CANARY verdict=fail conclusion=cancelled run=$child wall_seconds=1510
##[error]Process completed with exit code 1.
Post job cleanup.
Cleaning up orphan processes
EOF
  cat > "$fixture/authorization" <<EOF
##[group]Run bash scripts/authorize-release-rehearsal.sh
env:
  SOURCE_SHA: $source_sha
  REQUIRES_REHEARSAL: true
  REHEARSAL_RESULT: failure
  REHEARSAL_VERDICT: fail
  REHEARSAL_SOURCE_SHA: $source_sha
##[endgroup]
##[error]Process completed with exit code 1.
Post job cleanup.
Cleaning up orphan processes
EOF
}
observe() {
  release_failed_rehearsal_observation "$fixture/resolver" "$fixture/consumer" \
    "$fixture/authorization" "$source_sha" "$producer" "$child"
}
refuses() {
  if observe > /dev/null 2>&1; then
    echo "FAIL: accepted $1" >&2
    exit 1
  fi
}
reset_logs
observe | jq -e --arg source "$source_sha" \
  '.verdict == "failed_historical_rehearsal" and .source_sha == $source' >/dev/null

for mutation in duplicate_source missing_source wrong_source wrong_producer \
  wrong_child duplicate_verdict success_authorizer wrong_step truncated \
  unrelated_prose records_in_other_step missing_exit duplicate_env \
  missing_observe wrong_observe_child duplicate_observe wrong_order \
  duplicate_dispatch verdict_in_other_step; do
  reset_logs
  case "$mutation" in
    duplicate_source) sed '/  SOURCE_REVISION:/p' "$fixture/consumer" > "$fixture/changed" ;;
    missing_source) sed '/  SOURCE_REVISION:/d' "$fixture/consumer" > "$fixture/changed" ;;
    wrong_source) sed "s/$source_sha/0000000000000000000000000000000000000000/" "$fixture/consumer" > "$fixture/changed" ;;
    wrong_producer) sed "s/$producer/1/" "$fixture/resolver" > "$fixture/changed"; mv "$fixture/changed" "$fixture/resolver"; refuses "$mutation"; continue ;;
    wrong_child) sed "s/verdict=fail conclusion=cancelled run=$child/verdict=fail conclusion=cancelled run=1/" "$fixture/consumer" > "$fixture/changed" ;;
    duplicate_verdict) sed '/CONSUMER_CANARY verdict=/p' "$fixture/consumer" > "$fixture/changed" ;;
    success_authorizer) sed 's/REHEARSAL_RESULT: failure/REHEARSAL_RESULT: success/' "$fixture/authorization" > "$fixture/changed"; mv "$fixture/changed" "$fixture/authorization"; refuses "$mutation"; continue ;;
    wrong_step) sed 's/##\[group\]Run /##[group]Run echo /' "$fixture/consumer" > "$fixture/changed" ;;
    truncated) sed '/Cleaning up orphan processes/d' "$fixture/consumer" > "$fixture/changed" ;;
    unrelated_prose) sed 's/^CONSUMER_CANARY/quoted CONSUMER_CANARY/' "$fixture/consumer" > "$fixture/changed" ;;
    records_in_other_step) sed '/^CONSUMER_CANARY dispatched/i\
##[group]Run echo unrelated' "$fixture/consumer" > "$fixture/changed" ;;
    missing_exit) sed '/Process completed with exit code/d' "$fixture/consumer" > "$fixture/changed" ;;
    duplicate_env) sed '/^env:/p' "$fixture/consumer" > "$fixture/changed" ;;
    missing_observe) sed '/^##\[group\]Run CANARY_REPOSITORY=.*--observe$/s/--observe/--other/' "$fixture/consumer" > "$fixture/changed" ;;
    wrong_observe_child) sed "s/CANARY_RUN_ID: $child/CANARY_RUN_ID: 1/" "$fixture/consumer" > "$fixture/changed" ;;
    duplicate_observe) sed '/^##\[group\]Run CANARY_REPOSITORY=.*--observe$/p' "$fixture/consumer" > "$fixture/changed" ;;
    duplicate_dispatch) sed '/^CONSUMER_CANARY dispatched /p' "$fixture/consumer" > "$fixture/changed" ;;
    verdict_in_other_step) sed '/^CONSUMER_CANARY verdict=/i\
##[group]Run echo unrelated' "$fixture/consumer" > "$fixture/changed" ;;
    wrong_order) cat > "$fixture/changed" <<EOF
##[group]Run CANARY_REPOSITORY="\$CANARY_OWNER/\$CANARY_NAME" bash scripts/ci/consumer_canary.sh --observe
env:
  CANARY_RUN_ID: $child
  CANARY_STARTED_AT: 1700000000
  CANARY_WINDOW_SECONDS: 2700
  CANARY_DEADLINE_SECONDS: 7200
##[endgroup]
CONSUMER_CANARY verdict=fail conclusion=cancelled run=$child wall_seconds=1510
##[error]Process completed with exit code 1.
##[group]Run CANARY_REPOSITORY="\$CANARY_OWNER/\$CANARY_NAME" \\
env:
  SOURCE_REVISION: $source_sha
  CANARY_WORKFLOW: harn-repin-rehearsal.yml
##[endgroup]
CONSUMER_CANARY dispatched run=$child ref=default started_at=1700000000
Post job cleanup.
Cleaning up orphan processes
EOF
      ;;
  esac
  mv "$fixture/changed" "$fixture/consumer"
  refuses "$mutation"
done
reset_logs
for log in resolver consumer authorization; do
  sed 's/^/2026-10-06T21:18:39.9801450Z /' "$fixture/$log" > "$fixture/changed"
  mv "$fixture/changed" "$fixture/$log"
done
observe >/dev/null
node "$root/scripts/tests/failed_rehearsal_windows_test.mjs" "$root" "$fixture" "$source_sha" "$producer" "$child"
echo 'Historical rehearsal observation: split-step failure accepted; 19 false proofs refused.'
