#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
git init -q -b main "$tmp/source"
git -C "$tmp/source" config user.name 'Release fixture'
git -C "$tmp/source" config user.email 'release-fixture@example.invalid'
printf '[workspace.package]\nversion = "1.0.1"\n' > "$tmp/source/Cargo.toml"
git -C "$tmp/source" add Cargo.toml
git -C "$tmp/source" -c commit.gpgsign=false commit -qm 'Previous release'
git -C "$tmp/source" tag v1.0.0
git -C "$tmp/source" -c commit.gpgsign=false commit --allow-empty -qm 'Ordinary release'
ordinary_sha=$(git -C "$tmp/source" rev-parse HEAD)
git -C "$tmp/source" -c commit.gpgsign=false commit --allow-empty -qm 'Composed release' \
  -m $'Pairs-with: consumer#7\nPairs-with: consumer#8\nConsumer-composition: consumer#9'
composition_sha=$(git -C "$tmp/source" rev-parse HEAD)
git -C "$tmp/source" -c commit.gpgsign=false commit --allow-empty -qm 'Invalid declaration' \
  -m 'Consumer-composition: not-a-pull'
invalid_sha=$(git -C "$tmp/source" rev-parse HEAD)
sha=$ordinary_sha
authorize() {
  : > "$tmp/output"
  env SOURCE_SHA="$sha" SOURCE_CHECKOUT="$tmp/source" REQUIRES_REHEARSAL=true REHEARSAL_RESULT=success \
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
reuse() {
  repair REHEARSAL_RESULT=skipped REHEARSAL_VERDICT= REHEARSAL_SOURCE_SHA= REPAIR_REUSED=true "$@"
}
reuse || fail 'authenticated prior repair refused'
grep -Fxq 'ready=true' "$tmp/output" || fail 'reused repair emitted no authorization'
for refusal in REPAIR_REUSED=unknown REPAIR_REUSED=false REPAIR_RESULT= REPAIR_RESULT=failure \
  REPAIR_REQUEST= REPAIR_REQUEST=null CANDIDATE_RUN_ID=124 PRODUCER_ATTEMPT=2 CONSUMER_PULL=8 \
  REQUIRES_CONSUMER_REPAIR=false REQUIRES_REHEARSAL=false REHEARSAL_RESULT=failure \
  REHEARSAL_VERDICT=pass REHEARSAL_SOURCE_SHA="$sha"; do
  if reuse "$refusal"; then fail "accepted reused repair $refusal"; fi
  [[ ! -s "$tmp/output" ]] || fail "unauthorized reused repair output for $refusal"
done
sha=$composition_sha
# All existing recovery positives above must refuse the same valid results when
# the exact source declares composition, even if ambient text claims ordinary.
for route in authorize attach repair reuse; do
  request="$(jq -cn --arg source "$sha" \
    '{schema:"harn.release.consumer_repair.v1",context:{source_revision:$source},producer_run:"123",producer_attempt:1,consumer_pull:"7"}')"
  if "$route" PAIRING_TEXT='Ordinary release'; then fail "accepted composition $route recovery"; fi
  [[ ! -s "$tmp/output" ]] || fail "unauthorized composition output for $route"
  grep -Fq 'reason=composition_recovery_unsupported' "$tmp/log" \
    || fail "composition $route did not reach the source-bound refusal"
done
for flag in REQUIRES_REHEARSAL REQUIRES_ATTACHED_CONSUMER REQUIRES_CONSUMER_REPAIR REPAIR_REUSED; do
  if authorize REQUIRES_REHEARSAL=false REHEARSAL_RESULT=skipped "$flag=true"; then
    fail "accepted composition recovery flag $flag"
  fi
  [[ ! -s "$tmp/output" ]] || fail "unauthorized composition flag output for $flag"
  grep -Fq 'reason=composition_recovery_unsupported' "$tmp/log" \
    || fail "composition flag $flag bypassed source guard"
done
authorize REQUIRES_REHEARSAL=false REHEARSAL_RESULT=skipped \
  || fail 'producer-bound composition refused'
grep -Fxq 'ready=true' "$tmp/output" || fail 'composition producer positive emitted no authorization'
sha=$ordinary_sha
authorize PAIRING_TEXT=$'Pairs-with: consumer#7\nPairs-with: consumer#8\nConsumer-composition: consumer#9' \
  || fail 'ambient declaration replaced measured ordinary source'
for refusal in SOURCE_CHECKOUT= SOURCE_CHECKOUT="$tmp/missing" SOURCE_SHA="$invalid_sha" \
  SOURCE_SHA=0000000000000000000000000000000000000000; do
  if authorize "$refusal"; then fail "accepted unmeasured source $refusal"; fi
  [[ ! -s "$tmp/output" ]] || fail "unauthorized unmeasured-source output for $refusal"
done
node --input-type=module - "$root" <<'NODE'
import assert from 'node:assert/strict';
import fs from 'node:fs';
import {createRequire} from 'node:module';
const root=process.argv[2];
const yaml=createRequire(`${root}/package.json`)('js-yaml');
const workflow=yaml.load(fs.readFileSync(`${root}/.github/workflows/promote-release.yml`,'utf8'));
const steps=workflow.jobs.authorize.steps;
const source=steps.find(step=>step.with?.path==='release-source');
assert.equal(source.with.ref,'${{ needs.plan.outputs.source_sha }}');
assert.equal(source.with['fetch-depth'],0);
const authorize=steps.find(step=>step.run==='bash scripts/authorize-release-rehearsal.sh');
assert.equal(authorize.env.SOURCE_CHECKOUT,'${{ github.workspace }}/release-source');
assert.ok(steps.indexOf(source)<steps.indexOf(authorize));
NODE
echo 'release_rehearsal_authorization_test: ok'
