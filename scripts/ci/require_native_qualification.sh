#!/usr/bin/env bash
# Read-only publication qualification under the release platform policy.
native_script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ -f "$native_script_dir/../lib/native_qualification_receipt.sh" ]]; then
  # shellcheck source=scripts/lib/native_qualification_receipt.sh
  source "$native_script_dir/../lib/native_qualification_receipt.sh"
else
  # shellcheck source=scripts/lib/native_qualification_receipt.sh
  source "$native_script_dir/lib/native_qualification_receipt.sh"
fi

native_qualification_unmeasured() {
  jq -nc --arg stage "$1" '{kind:"harn.release.native_qualification",status:"unmeasured",stage:$stage,pending_count:null,failed_count:null}'
}

# Watch is only an observation aid; the caller's exact run and job census owns proof.
release_watch_run() {
  local repo=$1 id=$2 attempt diagnostic status
  [[ "$id" =~ ^[0-9]+$ ]] || return 1
  diagnostic=$(mktemp)
  for attempt in 1 2 3; do
    status=0
    gh run watch "$id" --repo "$repo" --exit-status > /dev/null 2> "$diagnostic" || status=$?
    if (( status == 0 )) || [[ ! -s "$diagnostic" ]]; then
      rm -f "$diagnostic"
      return 0
    fi
    # A permanent HTTP refusal takes precedence over incidental transient numbers.
    if grep -Eq 'HTTP 4[0-9][0-9](:|[[:space:]])' "$diagnostic" ||
      ! grep -Eq '^failed to get run: HTTP (500|502|503|504):' "$diagnostic" ||
      (( attempt == 3 )); then
      rm -f "$diagnostic"
      return 1
    fi
    printf 'Release observation interrupted; retrying the same run %s (%s/3).\n' "$id" "$attempt" >&2
    sleep 5
  done
}

require_native_platform() {
  local repo=$1 source=$2 policy=$3 workflow name metadata pages runs selected id attempt run jobs verdict revision discovery
  workflow=$(jq -r .workflow_path <<< "$policy")
  name=$(jq -r .name <<< "$policy")
  # A known positive workflow read precedes trusting an empty run census.
  if ! metadata=$(gh api "repos/$repo/actions/workflows/${workflow##*/}") ||
    ! jq -e --arg path "$workflow" '.path == $path and .state == "active" and (.id | type == "number" and . > 0 and floor == .)' <<< "$metadata" >/dev/null; then
    native_qualification_unmeasured "$name:workflow"
    return 1
  fi
  for ((discovery=0; ; discovery++)); do
    if ! pages=$(gh api --paginate --slurp "repos/$repo/actions/workflows/${workflow##*/}/runs?event=workflow_dispatch&branch=main&per_page=100") ||
    ! runs=$(jq -ce --arg source "$source" --arg repo "$repo" --arg path "$workflow" --argjson workflow "$(jq .id <<< "$metadata")" '
      if type != "array" or length == 0 or any(.[]; (.workflow_runs | type) != "array" or (.total_count | type) != "number")
      then error("unmeasured native run census")
      elif ([.[].total_count] | unique | length) != 1 then error("changing native run census")
      else . as $pages | [.[].workflow_runs[]]
        | if length == $pages[0].total_count and (map(.id) | unique | length) == length then . else error("partial native run census") end
        | if all(.[]; (.head_sha|test("^[0-9a-f]{40}$")) and .head_branch == "main" and .event == "workflow_dispatch" and .path == $path and .workflow_id == $workflow
          and .repository.full_name == $repo and .head_repository.full_name == $repo
          and (.id | type == "number" and . > 0 and floor == .)
          and (.run_attempt | type == "number" and . > 0 and floor == .)) then . else error("foreign native run") end
        | map(select(.display_title == ("Native qualification " + $source))) end
    ' <<< "$pages"); then
      native_qualification_unmeasured "$name:runs"
      return 1
    fi
    [[ $(jq length <<< "$runs") == 0 ]] || break
    if [[ ${WAIT_FOR_FULL_SUITE:-false} != true || $discovery -ge 24 ]]; then
      jq -nc --arg platform "$name" '{kind:"harn.release.native_qualification",platform:$platform,status:"missing",runs:0,pending_count:1,failed_count:0,missing:["exact-source workflow dispatch"]}'
      return 1
    fi
    # Dispatch acceptance can precede visibility in the run census.
    sleep 5
  done
  # A cancelled or skipped dispatch (often a duplicate) never decides; absence of any other run is unmeasured.
  runs=$(jq -c 'map(select(.status != "completed" or (.conclusion != "cancelled" and .conclusion != "skipped")))' <<< "$runs")
  if [[ $(jq length <<< "$runs") == 0 ]]; then
    native_qualification_unmeasured "$name:cancelled-only"
    return 1
  fi
  selected=$(jq -c 'max_by(.id)' <<< "$runs")
  id=$(jq -r .id <<< "$selected")
  attempt=$(jq -r .run_attempt <<< "$selected")
  revision=$(jq -r .head_sha <<< "$selected")
  if [[ ${WAIT_FOR_FULL_SUITE:-false} == true && $(jq -r .status <<< "$selected") != completed ]]; then
    # The final API records, rather than watch's exit status, decide proof.
    if ! release_watch_run "$repo" "$id"; then
      native_qualification_unmeasured "$name:watch"
      return 1
    fi
  fi
  if ! run=$(gh api "repos/$repo/actions/runs/$id/attempts/$attempt") ||
    ! jq -e --arg repo "$repo" --arg source "$source" --arg revision "$revision" --arg path "$workflow" --argjson id "$id" --argjson attempt "$attempt" --argjson workflow "$(jq .id <<< "$metadata")" '
      .id == $id and .run_attempt == $attempt and .head_sha == $revision and .head_branch == "main" and .display_title == ("Native qualification " + $source) and .path == $path
      and .workflow_id == $workflow and .event == "workflow_dispatch" and .repository.full_name == $repo and .head_repository.full_name == $repo
    ' <<< "$run" >/dev/null; then
    native_qualification_unmeasured "$name:attempt"
    return 1
  fi
  if ! pages=$(gh api --paginate --slurp "repos/$repo/actions/runs/$id/attempts/$attempt/jobs?per_page=100") ||
    ! jobs=$(jq -ce --arg source "$revision" --argjson id "$id" --argjson attempt "$attempt" '
      if type != "array" or length == 0 or any(.[]; (.jobs | type) != "array" or (.total_count | type) != "number")
      then error("unmeasured native job census")
      elif ([.[].total_count] | unique | length) != 1 then error("changing native job census")
      else . as $pages | [.[].jobs[]]
        | if length > 0 and length == $pages[0].total_count and (map(.id) | unique | length) == length then . else error("partial native job census") end
        | if all(.[]; .run_id == $id and .run_attempt == $attempt and .head_sha == $source
          and (.id | type == "number" and . > 0 and floor == .) and (.name | type == "string" and length > 0)
          and (.steps | type) == "array")
          then . else error("foreign native job") end end
    ' <<< "$pages"); then
    native_qualification_unmeasured "$name:jobs"
    return 1
  fi
  if ! verdict=$(jq -nce --argjson policy "$policy" --argjson run "$run" --argjson jobs "$jobs" --arg source "$source" '
    [$jobs[] | select(.name == $policy.required_job)] as $matches |
    (if ($matches | length) == 1 then $matches[0] else null end) as $job |
    [$policy.required_steps[] | . as $name |
      [($job.steps // [])[] | select(.name == $name)] as $steps |
      if ($steps | length) != 1 then {name:$name,status:"missing-or-duplicate",conclusion:null}
      else $steps[0] | {name,status,conclusion} end] as $steps |
    ([{name:"workflow run",status:$run.status,conclusion:$run.conclusion},
      {name:$policy.required_job,status:($job.status // "missing-or-duplicate"),conclusion:$job.conclusion}] + $steps) as $conditions |
    {kind:"harn.release.native_qualification",platform:$policy.name,source:$source,policy_source:$run.head_sha,run_id:$run.id,attempt:$run.run_attempt,
      observed_jobs:($jobs|length),required_steps:($steps|length),
      pending:[$conditions[]|select(.status != "completed")|.name],
      failures:[$conditions[]|select(.status == "completed" and .conclusion != "success")|.name],
      run_status:$run.status,run_conclusion:$run.conclusion,
      job_status:($job.status // "missing-or-duplicate"),job_conclusion:$job.conclusion,steps:$steps} |
    .pending_count = (.pending|length) | .failed_count = (.failures|length) |
    .status = (if .run_status == "completed" and .run_conclusion == "success" and .job_status == "completed"
      and .job_conclusion == "success" and .pending_count == 0 and .failed_count == 0 then "complete" else "refused" end)
  '); then
    native_qualification_unmeasured "$name:verdict"
    return 1
  fi
  # Failed execution is already a refusal. Only successful execution can have
  # produced the authenticated receipt required to qualify this source.
  if jq -e '.status == "complete"' <<< "$verdict" >/dev/null &&
    ! read_native_qualification_receipt "$repo" "$source" "$policy" "$run" >/dev/null; then
    native_qualification_unmeasured "$name:receipt"
    return 1
  fi
  printf '%s\n' "$verdict"
  jq -e '.status == "complete"' <<< "$verdict" >/dev/null
}

require_native_qualification() {
  local repo=$1 source=$2 policy_file=$3 policies platform failed=0
  if [[ ! "$repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ || ! "$source" =~ ^[0-9a-f]{40}$ ]]; then
    native_qualification_unmeasured identity
    return 1
  fi
  if ! policies=$(jq -ce '.platforms | if type == "array" and length > 0 and any(.[]; .required == true)
      and (map(.name)|unique|length) == length and all(.[]; (.name|type)=="string" and (.required|type)=="boolean"
        and (.workflow_path|test("^\\.github/workflows/[a-z0-9-]+\\.yml$")) and (.required_job|length)>0
        and (.required_steps|type)=="array" and (.required_steps|length)>0
        and (.required_steps|all(type=="string" and length>0)) and (.required_steps|unique|length)==(.required_steps|length))
      then . else error("unmeasured native platform policy") end' "$policy_file"); then
    native_qualification_unmeasured policy
    return 1
  fi
  while IFS= read -r platform; do
    require_native_platform "$repo" "$source" "$platform" || failed=1
  done <<< "$(jq -c '.[] | select(.required)' <<< "$policies")"
  [[ "$failed" == 0 ]]
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  set -euo pipefail
  if [[ $# != 3 ]]; then
    echo 'usage: require_native_qualification.sh OWNER/REPO SOURCE_SHA POLICY_JSON' >&2
    exit 2
  fi
  if [[ -n ${NATIVE_QUALIFICATION_STATUS_FILE:-} ]]; then
    jq -nc --arg source "$2" \
      '{kind:"harn.release.native_qualification.v1",stage:"native-qualification",source_sha:$source,
        verdict:"unmeasured",pending_count:null,failed_count:null,failing_jobs:[],pending_jobs:[],next_action:"retry-read"}' \
      > "$NATIVE_QUALIFICATION_STATUS_FILE"
  fi
  status=0
  qualification=$(require_native_qualification "$1" "$2" "$3") || status=$?
  verdict=$(jq -sc --arg source "$2" --argjson status "$status" '
    . as $platforms |
    {kind:"harn.release.native_qualification.v1",stage:"native-qualification",source_sha:$source,platforms:$platforms,
      pending_jobs:[$platforms[] | (.pending // .missing // [])[]],failing_jobs:[$platforms[].failures[]?]} |
    .pending_count = (if any($platforms[]; .pending_count == null) then null else [$platforms[].pending_count] | add end) |
    .failed_count = (if any($platforms[]; .failed_count == null) then null else [$platforms[].failed_count] | add end) |
    .verdict = (if ($platforms|length) == 0 or any($platforms[]; .status == "unmeasured") then "unmeasured"
      elif $status == 0 and all($platforms[]; .status == "complete") then "success"
      else "failure" end) |
    .next_action = (if .verdict == "success" then "continue" elif .verdict == "unmeasured" then "retry-read" else "inspect-native-qualification" end)
  ' <<< "$qualification")
  if [[ -n ${NATIVE_QUALIFICATION_STATUS_FILE:-} ]]; then
    printf '%s\n' "$verdict" > "$NATIVE_QUALIFICATION_STATUS_FILE"
  fi
  printf '%s\n' "$verdict"
  jq -e '.verdict == "success"' <<< "$verdict" >/dev/null || exit 1
  exit "$status"
fi
