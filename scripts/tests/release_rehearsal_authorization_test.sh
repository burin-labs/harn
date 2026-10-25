#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
sha=8743d2bd57df14eb1a022dec9cb842cbcbd7b568
authorize() {
  : > "$tmp/output"
  env SOURCE_SHA="$sha" REQUIRES_REHEARSAL=true REHEARSAL_RESULT=success \
    REHEARSAL_VERDICT=pass REHEARSAL_SOURCE_SHA="$sha" GITHUB_OUTPUT="$tmp/output" \
    "$@" bash "$root/scripts/authorize-release-rehearsal.sh" > "$tmp/log" 2>&1
}
fail() { echo "FAIL: $*" >&2; exit 1; }
authorize || fail 'measured exact-source rehearsal refused'
grep -Fxq 'ready=true' "$tmp/output" || fail 'positive emitted no authorization'
authorize REQUIRES_REHEARSAL=false REHEARSAL_RESULT=skipped REHEARSAL_VERDICT= REHEARSAL_SOURCE_SHA= \
  || fail 'producer-qualified candidate refused'
for refusal in REQUIRES_REHEARSAL= REQUIRES_REHEARSAL=unknown SOURCE_SHA= \
  REHEARSAL_RESULT= REHEARSAL_RESULT=skipped REHEARSAL_RESULT=failure \
  REHEARSAL_RESULT=cancelled REHEARSAL_VERDICT= REHEARSAL_VERDICT=fail \
  REHEARSAL_SOURCE_SHA= REHEARSAL_SOURCE_SHA=0000000000000000000000000000000000000000; do
  if authorize "$refusal"; then fail "accepted $refusal"; fi
  [[ ! -s "$tmp/output" ]] || fail "unauthorized output for $refusal"
done
if authorize REQUIRES_REHEARSAL=false; then fail 'unexpected replacement rehearsal accepted'; fi
attach() {
  authorize REQUIRES_REHEARSAL=false REHEARSAL_RESULT=skipped \
    REQUIRES_ATTACHED_CONSUMER=true ATTACHED_RESULT=success \
    ATTACHED_SOURCE_SHA="$sha" CANDIDATE_RUN_ID=123 ATTACHED_PRODUCER_RUN=123 "$@"
}
attach || fail 'authenticated completed child refused'
grep -Fxq 'ready=true' "$tmp/output" || fail 'attached positive emitted no authorization'
for refusal in REQUIRES_ATTACHED_CONSUMER=unknown ATTACHED_RESULT= ATTACHED_RESULT=failure \
  ATTACHED_RESULT=skipped ATTACHED_SOURCE_SHA= ATTACHED_SOURCE_SHA=0000000000000000000000000000000000000000 \
  CANDIDATE_RUN_ID=0 ATTACHED_PRODUCER_RUN=124 REHEARSAL_RESULT=success REQUIRES_REHEARSAL=true; do
  if attach "$refusal"; then fail "accepted attached $refusal"; fi
  [[ ! -s "$tmp/output" ]] || fail "unauthorized attached output for $refusal"
done
if authorize ATTACHED_RESULT=success; then fail 'unexpected attached result accepted'; fi
request="$(jq -cn --arg source "$sha" \
  '{schema:"harn.release.consumer_repair.v1",context:{source_revision:$source},producer_run:"123",producer_attempt:1,consumer_pull:"7"}')"
repair() {
  authorize REQUIRES_CONSUMER_REPAIR=true REPAIR_RESULT=success REPAIR_REQUEST="$request" \
    CANDIDATE_RUN_ID=123 PRODUCER_ATTEMPT=1 CONSUMER_PULL=7 "$@"
}
repair || fail 'authenticated archive repair refused'
grep -Fxq 'ready=true' "$tmp/output" || fail 'repair positive emitted no authorization'
for refusal in REQUIRES_CONSUMER_REPAIR=unknown REPAIR_RESULT= REPAIR_RESULT=skipped \
  REPAIR_RESULT=failure REPAIR_REQUEST= REPAIR_REQUEST=null REPAIR_REQUEST='{}' \
  CANDIDATE_RUN_ID=124 PRODUCER_ATTEMPT=2 CONSUMER_PULL=8 REQUIRES_REHEARSAL=false; do
  if repair "$refusal"; then fail "accepted repair $refusal"; fi
  [[ ! -s "$tmp/output" ]] || fail "unauthorized repair output for $refusal"
done
if authorize REPAIR_RESULT=success REPAIR_REQUEST="$request"; then fail 'unexpected repair accepted'; fi
echo 'release_rehearsal_authorization_test: ok'
