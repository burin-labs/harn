#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$root/scripts/lib/release_consumer_verdict.sh"
source "$root/scripts/lib/candidate_archive_contract.sh"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
source_sha=1111111111111111111111111111111111111111
child_head=2222222222222222222222222222222222222222
targets="$(candidate_archive_expected_targets_json)"
producer="$(jq -nc --arg sha "$source_sha" '{id:100,run_attempt:1,
  repository:{full_name:"burin-labs/harn"},head_repository:{full_name:"burin-labs/harn"},
  head_sha:$sha,path:".github/workflows/build-release-binaries.yml",event:"merge_group",
  status:"completed",conclusion:"failure"}')"
producer_jobs="$(jq -nc --argjson targets "$targets" '
  ["Resolve release context","Prepare CLI AOT payload","Assemble candidate manifest",
    "Release residual audit","Consumer release rehearsal / Decide whether main moved"] +
  ($targets|map("Build " + .)) +
  (["linux","linux-arm64","windows","macos"]|map("Release smoke / Release smoke (" + . + ")")) |
  map({name:.,conclusion:"success"}) +
  [{name:"Consumer release rehearsal / Consumer canary",conclusion:"failure"},
   {name:"Release candidate verdict",conclusion:"failure"},
   {name:"Consumer release rehearsal / Settled verdict",conclusion:"skipped"},
   {name:"Report cache budget",conclusion:"skipped"}] |
  to_entries|map(.value+{id:(.key+1),run_id:100,run_attempt:1,status:"completed"}) |
  [{total_count:length,jobs:.}]')"
child="$(jq -nc --arg sha "$child_head" '{id:200,run_attempt:1,
  repository:{full_name:"example/product"},head_repository:{full_name:"example/product"},
  head_sha:$sha,head_branch:"main",path:".github/workflows/harn-repin-rehearsal.yml",
  event:"workflow_dispatch",status:"completed",conclusion:"success"}')"
child_jobs="$(jq -nc '
  [["Rehearse the Harn repin surface","Rehearse a real Harn repin"],
   ["Prove the candidate against the harn-linked TUI suite","Run the harn-linked TUI suite against the candidate"],
   ["Prove the candidate against the first-run gauntlet","Run the first-run gauntlet against the candidate"],
   ["Harn repin rehearsal","Refuse a candidate its legs did not prove"]] |
  to_entries|map({id:(.key+1),run_id:200,run_attempt:1,name:.value[0],status:"completed",conclusion:"success",
    steps:[{name:.value[1],status:"completed",conclusion:"success"}]}) |
  [{total_count:length,jobs:.}]')"
producer_jobs="$(jq -c '.[0].jobs |= map(. + {steps:[{
  name:(if .name=="Consumer release rehearsal / Consumer canary" then "Observe the final bounded window"
    elif .name=="Release candidate verdict" then "Require every candidate job to pass" else "Owning proof" end),
  status:"completed",conclusion:(if .conclusion=="failure" then "failure" else "success" end)}]})' <<< "$producer_jobs")"
fixture_step() {
  local file=$1 command=$2
  shift 2
  {
    printf '##[group]Run %s\nenv:\n' "$command"
    printf '  %s\n' "$@"
    printf '##[endgroup]\n'
  } >> "$scratch/$file"
}
# shellcheck disable=SC2016,SC1003 # exact owning workflow command
fixture_step consumer 'CANARY_REPOSITORY="$CANARY_OWNER/$CANARY_NAME" \' \
  "SOURCE_REVISION: $source_sha" 'CANARY_WORKFLOW: harn-repin-rehearsal.yml'
printf 'CONSUMER_CANARY dispatched run=200 ref=default started_at=1000\n' >> "$scratch/consumer"
for seconds in 2700 5400 7201; do
  # shellcheck disable=SC2016 # exact command, no ambient expansion
  fixture_step consumer 'CANARY_REPOSITORY="$CANARY_OWNER/$CANARY_NAME" bash scripts/ci/consumer_canary.sh --observe' \
    'CANARY_RUN_ID: 200' 'CANARY_STARTED_AT: 1000' \
    'CANARY_WINDOW_SECONDS: 2700' 'CANARY_DEADLINE_SECONDS: 7200'
  if [[ $seconds == 7201 ]]; then
    printf '##[error]CONSUMER_CANARY reason=no_verdict_before_deadline run=200 verdict=unmeasured wall_seconds=7201\n##[error]Process completed with exit code 1.\n' >> "$scratch/consumer"
  else
    printf 'CONSUMER_CANARY pending run=200 status=in_progress wall_seconds=%s\n' "$seconds" >> "$scratch/consumer"
  fi
done
printf 'Post job cleanup.\nCleaning up orphan processes\n' >> "$scratch/consumer"
fixture_step final 'set -euo pipefail' 'SETUP_RESULT: success' 'BUILD_MODE: candidate' \
  'CANDIDATE_PURPOSE: release' "REHEARSAL_SOURCE_SHA: $source_sha" 'CONSUMER_RESULT: failure'
sed '/^##\[endgroup\]$/i\
  RESULTS: {\
  "consumer-rehearsal": {\
    "result": "failure"\
  }\
}' "$scratch/final" > "$scratch/final-multiline"
mv "$scratch/final-multiline" "$scratch/final"
printf '##[error]Consumer release rehearsal at %s finished failure; publication refused.\n##[error]Process completed with exit code 1.\nCleaning up orphan processes\n' "$source_sha" >> "$scratch/final"
linked="$(jq -nc --arg source "$source_sha" '{revision:$source,suite:"harn-linked",testsRun:123,
  observation:{outcome:"passed",producerExitCode:0,logExitCode:0}}')"
gauntlet="$(jq -nc --arg source "$source_sha" '{revision:$source,suite:"first-run-gauntlet",testsRun:5}')"
fixture_step verdict 'node --experimental-strip-types --no-warnings scripts/check-pretag-candidate-leg.ts' \
  "PRETAG_SOURCE_REVISION: $source_sha" 'PRETAG_LEG_RESULT: success' "PRETAG_LEG_RECEIPT: $linked" \
  'PRETAG_GAUNTLET_LEG_RESULT: success' "PRETAG_GAUNTLET_LEG_RECEIPT: $gauntlet"
printf 'The candidate cleared both measured suites.\nPost job cleanup.\nCleaning up orphan processes\n' >> "$scratch/verdict"
fail() { echo "FAIL: $*" >&2; exit 1; }
judge_producer() { release_late_consumer_producer_census "$producer" "$producer_jobs" burin-labs/harn 100 "$source_sha" "$targets"; }
judge_child() { release_completed_consumer_census "$child" "$child_jobs" example/product 200; }
judge_observation() { release_completed_consumer_observation "$scratch/consumer" "$scratch/verdict" "$source_sha" 200; }
judge_deadline() { release_late_consumer_deadline_observation "$scratch/consumer" "$scratch/final" "$source_sha" 200 "${clock:-1000}"; }
judge_producer > "$scratch/producer" || fail 'complete archive/smoke/audit producer refused'
judge_child > "$scratch/child" || fail 'completed product proof refused'
jq -e '.pending==0 and (.failed_jobs|length)==2' "$scratch/producer" >/dev/null
jq -e '.observed==4 and .pending==0 and .bad==0' "$scratch/child" >/dev/null
judge_observation > "$scratch/observation" || fail 'acknowledged exact dispatch refused'
judge_deadline > "$scratch/deadline" || fail 'bounded original deadline refusal not identified'
jq -e '.original_observation=="deadline_unmeasured"' "$scratch/deadline" >/dev/null
cp "$scratch/final" "$scratch/final-good"
sed '/^  CONSUMER_RESULT: failure$/a\
unexpected continuation' "$scratch/final-good" > "$scratch/final"
if judge_deadline > "$scratch/refused" 2>&1; then fail 'multiline required scalar accepted'; fi
sed '/^  CONSUMER_RESULT: failure$/a\
  CONSUMER_RESULT: failure' "$scratch/final-good" > "$scratch/final"
if judge_deadline > "$scratch/refused" 2>&1; then fail 'duplicate runner field accepted'; fi
sed '/^env:$/a\
orphan continuation' "$scratch/final-good" > "$scratch/final"
if judge_deadline > "$scratch/refused" 2>&1; then fail 'orphan env continuation accepted'; fi
cp "$scratch/final-good" "$scratch/final"
fixture_root=$scratch fixture_producer=$producer fixture_child=$child
fixture_producer_jobs=$producer_jobs fixture_child_jobs=$child_jobs
gh() {
  local endpoint=${*: -1}
  printf '%s\n' "$endpoint" >> "$fixture_root/api-calls"
  case "$endpoint" in
    repos/burin-labs/harn/*) [[ "$GH_TOKEN" == producer-read ]] || return 1 ;;
    repos/example/product/*) [[ "$GH_TOKEN" == child-read ]] || return 1 ;;
    *) return 1 ;;
  esac
  case "$endpoint" in
    repos/burin-labs/harn/actions/runs/100)
      if [[ "${adapter_mode:-}" == producer-final-attempt &&
        "$(grep -Fxc "$endpoint" "$fixture_root/api-calls")" == 2 ]]; then
        jq -c '.run_attempt=2' <<< "$fixture_producer"
      elif [[ "${adapter_mode:-}" == producer-final-status &&
        "$(grep -Fxc "$endpoint" "$fixture_root/api-calls")" == 2 ]]; then
        jq -c '.status="in_progress"|.conclusion=null' <<< "$fixture_producer"
      else printf '%s\n' "$fixture_producer"; fi ;;
    'repos/burin-labs/harn/actions/runs/100/jobs?filter=latest&per_page=100') printf '%s\n' "$fixture_producer_jobs" ;;
    repos/burin-labs/harn/actions/jobs/15/logs) cat "$fixture_root/consumer" ;;
    repos/burin-labs/harn/actions/jobs/16/logs) cat "$fixture_root/final" ;;
    repos/burin-labs/harn/compare/*...main)
      jq -nc --arg sha "$source_sha" '{status:"identical",merge_base_commit:{sha:$sha}}' ;;
    repos/example/product/actions/runs/200)
      [[ "${adapter_mode:-}" != http-failure ]] || return 1
      if [[ "${adapter_mode:-}" == child-final-attempt &&
        "$(grep -Fxc "$endpoint" "$fixture_root/api-calls")" == 2 ]]; then
        jq -c '.run_attempt=2' <<< "$fixture_child"
      elif [[ "${adapter_mode:-}" == child-final-status &&
        "$(grep -Fxc "$endpoint" "$fixture_root/api-calls")" == 2 ]]; then
        jq -c '.conclusion="cancelled"' <<< "$fixture_child"
      elif [[ "${adapter_mode:-}" == pending ]]; then jq -c '.status="in_progress"|.conclusion=null' <<< "$fixture_child"
      else printf '%s\n' "$fixture_child"; fi ;;
    'repos/example/product/actions/runs/200/jobs?filter=latest&per_page=100')
      if [[ "${adapter_mode:-}" == partial ]]; then jq -c '.[0].total_count+=1' <<< "$fixture_child_jobs"
      else printf '%s\n' "$fixture_child_jobs"; fi ;;
    repos/example/product/actions/jobs/4/logs) cat "$fixture_root/verdict" ;;
    *) echo 'Unexpected fixture API call' >&2; return 1 ;;
  esac
}
export GH_TOKEN=child-read REHEARSAL_PROMOTION_READ_TOKEN=producer-read
judge_adapter() { release_authenticated_late_consumer burin-labs/harn 100 "$source_sha" example/product 200; }
judge_adapter > "$scratch/authorization" || fail 'same-path authenticated recovery refused'
jq -e '.schema=="burin-labs.late-consumer-authorization.v1" and
  .producer_run==100 and .consumer_run==200 and .producer_attempt==1 and .consumer_attempt==1 and
  .linked_receipt.testsRun==123 and .gauntlet_receipt.testsRun==5 and .pending==0 and
  (has("consumer_repository")|not)' "$scratch/authorization" >/dev/null
[[ -s "$scratch/api-calls" ]] || fail 'no actual transport requests reached'
export -f gh
export fixture_root fixture_producer fixture_child fixture_producer_jobs fixture_child_jobs source_sha
env GH_TOKEN=producer-read GITHUB_REPOSITORY=burin-labs/harn CANDIDATE_RUN_ID=100 \
  EXPECTED_SOURCE_SHA="$source_sha" COMPLETED_CONSUMER_RUN_ID=200 GITHUB_OUTPUT="$scratch/resolved" \
  bash "$root/scripts/resolve-release-promotion-source.sh" > "$scratch/resolver-log" 2>&1 \
  || fail 'canonical resolver refused eligible late-consumer source'
grep -Fxq 'requires_attached_consumer=true' "$scratch/resolved" || fail 'resolver did not require attached proof'
for completed_child in '' invalid; do
  : > "$scratch/resolver-refused"
  if env GH_TOKEN=producer-read GITHUB_REPOSITORY=burin-labs/harn CANDIDATE_RUN_ID=100 \
    EXPECTED_SOURCE_SHA="$source_sha" COMPLETED_CONSUMER_RUN_ID="$completed_child" GITHUB_OUTPUT="$scratch/resolver-refused" \
    bash "$root/scripts/resolve-release-promotion-source.sh" > "$scratch/resolver-refused-log" 2>&1; then
    fail 'canonical resolver accepted failed producer without valid attachment request'
  fi
  [[ ! -s "$scratch/resolver-refused" ]] || fail 'refused resolver emitted a source'
done
env GITHUB_REPOSITORY=burin-labs/harn GITHUB_RUN_ID=900 GITHUB_RUN_ATTEMPT=2 \
  CANDIDATE_RUN_ID=100 SOURCE_SHA="$source_sha" CANARY_REPOSITORY=example/product \
  COMPLETED_CONSUMER_RUN_ID=200 AUTHORIZATION_FILE="$scratch/cli-authorization.json" \
  bash "$root/scripts/authorize-completed-consumer.sh" > "$scratch/cli-log" 2>&1 \
  || fail 'actual authorization entrypoint refused complete proof'
jq -e '.authorization_run==900 and .authorization_attempt==2 and .producer_run==100 and .pending==0' \
  "$scratch/cli-authorization.json" >/dev/null || fail 'entrypoint wrote unbound proof'
if env GITHUB_RUN_ID=900 GITHUB_RUN_ATTEMPT=2 AUTHORIZATION_FILE="$scratch/cli-authorization.json" \
  bash "$root/scripts/authorize-completed-consumer.sh" > "$scratch/cli-stale" 2>&1; then
  fail 'entrypoint overwrote prior authorization'
fi
if env GITHUB_REPOSITORY=burin-labs/harn GITHUB_RUN_ID=900 GITHUB_RUN_ATTEMPT=2 \
  CANDIDATE_RUN_ID=100 SOURCE_SHA="$source_sha" CANARY_REPOSITORY=example/product \
  COMPLETED_CONSUMER_RUN_ID=200 AUTHORIZATION_FILE="$scratch/cli-refused.json" adapter_mode=pending \
  bash "$root/scripts/authorize-completed-consumer.sh" > "$scratch/cli-refused-log" 2>&1; then
  fail 'entrypoint admitted unfinished child'
fi
[[ ! -e "$scratch/cli-refused.json" ]] || fail 'refused entrypoint published partial proof'
for adapter_mode in pending partial http-failure; do
  if judge_adapter > "$scratch/refused" 2>&1; then fail "adapter $adapter_mode accepted"; fi
done
adapter_mode=
for adapter_mode in child-final-attempt child-final-status producer-final-attempt producer-final-status; do
  : > "$scratch/api-calls"
  if judge_adapter > "$scratch/refused" 2>&1; then fail "adapter $adapter_mode accepted"; fi
  case "$adapter_mode" in
    child-*) endpoint=repos/example/product/actions/runs/200 ;;
    producer-*) endpoint=repos/burin-labs/harn/actions/runs/100 ;;
  esac
  [[ "$(grep -Fxc "$endpoint" "$scratch/api-calls")" == 2 ]] || fail 'final-read control did not fire'
done
adapter_mode=
clock=1001
if judge_deadline > "$scratch/refused" 2>&1; then fail 'different dispatch clock accepted'; fi
clock=1000
cp "$scratch/consumer" "$scratch/good-consumer"
cp "$scratch/verdict" "$scratch/good-verdict"
sed 's/wall_seconds=7201/wall_seconds=7199/' "$scratch/good-consumer" > "$scratch/consumer"
if judge_deadline > "$scratch/refused" 2>&1; then fail 'early deadline accepted'; fi
cp "$scratch/good-consumer" "$scratch/consumer"
sed 's/reason=no_verdict_before_deadline/reason=consumer_rehearsal_failed/' "$scratch/good-consumer" > "$scratch/consumer"
if judge_deadline > "$scratch/refused" 2>&1; then fail 'real consumer failure accepted'; fi
cp "$scratch/good-consumer" "$scratch/consumer"
sed 's/producerExitCode":0/producerExitCode":1/' "$scratch/good-verdict" > "$scratch/verdict"
if judge_observation > "$scratch/refused" 2>&1; then fail 'failed suite receipt accepted'; fi
cp "$scratch/good-verdict" "$scratch/verdict"
if release_completed_consumer_observation "$scratch/consumer" "$scratch/verdict" "$child_head" 200 \
  > "$scratch/refused" 2>&1; then fail 'different source accepted'; fi
if release_completed_consumer_observation "$scratch/consumer" "$scratch/verdict" "$source_sha" 201 \
  > "$scratch/refused" 2>&1; then fail 'different child accepted'; fi
good_producer=$producer good_producer_jobs=$producer_jobs good_child=$child good_child_jobs=$child_jobs
for mutation in '.status="in_progress"' '.conclusion=null' '.head_sha="0000000000000000000000000000000000000000"' \
  '.event="pull_request"' '.repository.full_name="foreign/repo"' '.run_attempt=2'; do
  producer="$(jq -c "$mutation" <<< "$good_producer")"
  if judge_producer > "$scratch/refused" 2>&1; then fail "producer $mutation accepted"; fi
done
producer=$good_producer
for mutation in '.[0].total_count+=1' '.[0].jobs[0].conclusion="failure"' \
  '.[0].jobs[0].conclusion="skipped"' '.[0].jobs[0].status="in_progress"' \
  '.[0].jobs[0].run_attempt=2' '.[0].jobs[0].name="unrelated"' \
  '.[0].jobs[1].id=.[0].jobs[0].id' '.[0].jobs[0].conclusion=null' '[]'; do
  producer_jobs="$(jq -c "$mutation" <<< "$good_producer_jobs")"
  if judge_producer > "$scratch/refused" 2>&1; then fail "producer census $mutation accepted"; fi
done
producer_jobs=$good_producer_jobs
for mutation in '.status="in_progress"' '.conclusion="failure"' '.head_branch="feature"' \
  '.head_repository.full_name="fork/product"' '.event="pull_request"' '.run_attempt=2'; do
  child="$(jq -c "$mutation" <<< "$good_child")"
  if judge_child > "$scratch/refused" 2>&1; then fail "child $mutation accepted"; fi
done
child=$good_child
for mutation in '.[0].total_count+=1' '.[0].jobs[0].conclusion="skipped"' \
  '.[0].jobs[0].steps=[]' '.[0].jobs[0].steps[0].conclusion="failure"' \
  '.[0].jobs[0].steps[0].name="unrelated"' '.[0].jobs[0].run_id=999' \
  '.[0].jobs[0].run_attempt=2' '.[0].jobs[0].name="unrelated"' \
  '.[0].jobs[1].id=.[0].jobs[0].id' '[]'; do
  child_jobs="$(jq -c "$mutation" <<< "$good_child_jobs")"
  if judge_child > "$scratch/refused" 2>&1; then fail "child census $mutation accepted"; fi
done
echo 'completed_consumer_recovery_test: authenticated same-path/CLI positives, 44 reader refusals and stale/partial writer refusals passed'
