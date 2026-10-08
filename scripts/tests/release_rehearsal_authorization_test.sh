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
echo 'release_rehearsal_authorization_test: ok'
