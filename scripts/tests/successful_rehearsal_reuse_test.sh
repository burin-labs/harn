#!/usr/bin/env bash
# shellcheck disable=SC2031 # authority changes remain inside the owning subshell
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fixture_root="$(mktemp -d)"
trap 'rm -rf "$fixture_root"' EXIT
# shellcheck source=scripts/lib/release_consumer_verdict.sh
source "${RELEASE_REUSE_LIBRARY:-$root/scripts/lib/release_consumer_verdict.sh}"
source_sha=1111111111111111111111111111111111111111
consumer_sha=2222222222222222222222222222222222222222
export GH_TOKEN=consumer-read REHEARSAL_PROMOTION_READ_TOKEN=promotion-read

fixture_step() {
  local file=$1 command=$2
  shift 2
  printf '##[group]Run %s\nenv:\n' "$command" > "$fixture_root/$file"
  printf '  %s\n' "$@" >> "$fixture_root/$file"
  printf '##[endgroup]\n' >> "$fixture_root/$file"
}
fixture_step resolver 'bash scripts/resolve-release-promotion-source.sh' \
  'CANDIDATE_RUN_ID: 200' "EXPECTED_SOURCE_SHA: $source_sha"
printf 'Post job cleanup.\nCleaning up orphan processes\n' >> "$fixture_root/resolver"
fixture_step authorization 'bash scripts/authorize-release-rehearsal.sh' \
  "SOURCE_SHA: $source_sha" 'REQUIRES_REHEARSAL: true' 'REHEARSAL_RESULT: success' \
  'REHEARSAL_VERDICT: pass' "REHEARSAL_SOURCE_SHA: $source_sha"
printf 'Post job cleanup.\nCleaning up orphan processes\n' >> "$fixture_root/authorization"
# shellcheck disable=SC2016,SC1003 # literal owning runner command
fixture_step consumer 'CANARY_REPOSITORY="$CANARY_OWNER/$CANARY_NAME" \' \
  "SOURCE_REVISION: $source_sha" 'CANARY_WORKFLOW: harn-repin-rehearsal.yml'
cat >> "$fixture_root/consumer" <<'EOF'
CONSUMER_CANARY dispatched run=300 ref=default started_at=1000
##[group]Run CANARY_REPOSITORY="$CANARY_OWNER/$CANARY_NAME" bash scripts/ci/consumer_canary.sh --observe
env:
  CANARY_RUN_ID: 300
  CANARY_STARTED_AT: 1000
  CANARY_WINDOW_SECONDS: 2700
  CANARY_DEADLINE_SECONDS: 7200
##[endgroup]
CONSUMER_CANARY verdict=pass conclusion=success run=300 wall_seconds=20
##[end-action id=observe-1.observe;outcome=success;conclusion=success;duration_ms=20]
Post job cleanup.
Cleaning up orphan processes
EOF

gh() {
  local path=${*: -1}
  case "$*" in
    *'/actions/jobs/1/logs') cat "$fixture_root/resolver" ;;
    *'/actions/jobs/2/logs') cat "$fixture_root/consumer" ;;
    *'/actions/jobs/3/logs') cat "$fixture_root/authorization" ;;
    *'/actions/runs/100/jobs?'*)
      jq -nc --arg mode "${mode:-}" '[{total_count:(if $mode == "partial" then 4 else 3 end),jobs:[
        {id:1,run_id:100,run_attempt:1,name:"Resolve certified source",status:"completed",conclusion:"success"},
        {id:2,run_id:100,run_attempt:1,name:"Recover missing consumer rehearsal / Consumer canary",status:"completed",conclusion:(if $mode == "failed-parent-job" then "failure" else "success" end)},
        {id:3,run_id:100,run_attempt:1,name:"Require measured consumer completion",status:"completed",conclusion:"success"}]}]' ;;
    *'/actions/runs/300/jobs?'*)
      jq -nc --arg mode "${mode:-}" '[{total_count:3,jobs:[
        {id:11,run_id:300,run_attempt:1,name:"Prove the candidate against the harn-linked TUI suite",status:"completed",conclusion:"success"},
        {id:12,run_id:300,run_attempt:1,name:"Prove the candidate against the first-run gauntlet",status:"completed",conclusion:"success"},
        {id:13,run_id:300,run_attempt:1,name:(if $mode == "missing-product" then "Other" else "Rehearse the Harn repin surface" end),status:"completed",conclusion:"success"}]}]' ;;
    *'/actions/runs/100')
      [[ "$GH_TOKEN" == promotion-read ]] || return 1
      printf '{"id":100,"repository":{"full_name":"burin-labs/harn"},"head_repository":{"full_name":"burin-labs/harn"},"head_branch":"main","path":".github/workflows/promote-release.yml","event":"workflow_dispatch","status":"completed","conclusion":"failure","run_attempt":1}\n' ;;
    *'/actions/runs/300')
      [[ "$GH_TOKEN" == consumer-read ]] || return 1
      jq -nc --arg sha "$consumer_sha" --arg mode "${mode:-}" '{id:300,repository:{full_name:"burin-labs/fixture-product"},head_repository:{full_name:"burin-labs/fixture-product"},head_sha:$sha,head_branch:"main",path:".github/workflows/harn-repin-rehearsal.yml",event:"workflow_dispatch",status:(if $mode == "pending" then "in_progress" else "completed" end),conclusion:(if $mode == "failed-child" then "failure" else "success" end),run_attempt:1}' ;;
    *'/commits/main --jq .sha')
      if [[ "${mode:-}" == changed-head ]]; then printf '%040d\n' 9; else printf '%s\n' "$consumer_sha"; fi ;;
    *'repos/burin-labs/fixture-product --jq .default_branch') printf 'main\n' ;;
    'workflow run '*) touch "$fixture_root/dispatched"; return 1 ;;
    *) echo "Unexpected fake API: $path" >&2; return 1 ;;
  esac
}
reuse() { release_authenticated_successful_rehearsal burin-labs/harn 100 "${producer:-200}" "${source:-$source_sha}" burin-labs/fixture-product; }
[[ "$(reuse)" == 300 ]]
for mode in partial failed-parent-job pending failed-child changed-head missing-product; do
  if reuse > "$fixture_root/result" 2>&1; then echo "accepted $mode" >&2; exit 1; fi
done
mode=
producer=201
if reuse > "$fixture_root/result" 2>&1; then echo 'accepted wrong producer' >&2; exit 1; fi
producer=200
source=3333333333333333333333333333333333333333
if reuse > "$fixture_root/result" 2>&1; then echo 'accepted wrong source' >&2; exit 1; fi
source=$source_sha
# Reach the actual driver: successful recovery emits the existing child ID and
# never dispatches; a refused authenticated helper terminates before dispatch.
# shellcheck source=scripts/ci/consumer_canary.sh
source "$root/scripts/ci/consumer_canary.sh"
export GITHUB_REPOSITORY=burin-labs/harn CANARY_REPOSITORY=burin-labs/fixture-product
export CANARY_WORKFLOW=harn-repin-rehearsal.yml SOURCE_REVISION="$source_sha" WORKSPACE_VERSION=0.10.161
export REHEARSAL_PROMOTION_RUN_ID=100 CANDIDATE_RUN_ID=200
canary_dispatch > "$fixture_root/driver-result"
[[ "$CANARY_RUN_ID" == 300 && "$CANARY_STARTED_AT" =~ ^[1-9][0-9]*$ ]]
grep -Fq 'CONSUMER_CANARY reused run=300 promotion=100' "$fixture_root/driver-result"
[[ ! -e "$fixture_root/dispatched" ]]
mode=pending
if (canary_dispatch) > "$fixture_root/driver-refused" 2>&1; then echo 'driver accepted pending child' >&2; exit 1; fi
grep -Fq 'reason=recovered_rehearsal_unqualified' "$fixture_root/driver-refused"
[[ ! -e "$fixture_root/dispatched" ]]
mode=
printf 'unexpected post-verdict output\n' >> "$fixture_root/consumer"
if reuse > "$fixture_root/result" 2>&1; then echo 'accepted incomplete log' >&2; exit 1; fi
echo 'successful_rehearsal_reuse_test: 12 controls passed'
