#!/usr/bin/env bash

# Promotion reads the actual job, including legacy runs whose workflow stayed
# green after a tolerated consumer failure. Empty and partial reads refuse.
release_consumer_verdict() {
  local repository="${1:?repository required}"
  local run_id="${2:?candidate run required}"
  local sha="${3:?candidate commit required}"
  local run pages verdict
  run="$(gh api "repos/$repository/actions/runs/$run_id")" || return 1
  if ! jq -e --arg sha "$sha" --arg run "$run_id" '
    (.id | tostring) == $run and .head_sha == $sha and
    .status == "completed" and .conclusion == "success"
  ' <<< "$run" >/dev/null; then
    echo "::error::Candidate run $run_id at $sha is mismatched or not successful; consumer publication gate refused." >&2
    return 1
  fi
  pages="$(gh api --paginate --slurp "repos/$repository/actions/runs/$run_id/jobs?filter=latest&per_page=100")" || return 1
  verdict="$(jq -cer '
    if type != "array" or length == 0 or
       any(.[]; (.jobs | type) != "array" or (.total_count | type) != "number"
         or .total_count < 0 or .total_count != (.total_count | floor))
    then error("missing consumer job census") else . end |
    . as $pages | [.[].jobs[]] as $jobs |
    if all($pages[]; .total_count == ($jobs | length)) then $jobs
    else error("partial consumer job census") end |
    # Absence of the consumer is meaningful only among actual identified jobs.
    if length == 0 or any(.[];
      type != "object" or
      (.id | type) != "number" or .id <= 0 or .id != (.id | floor) or
      (.name | type) != "string" or (.name | test("\\S") | not) or
      (.status | type) != "string" or (.status | test("\\S") | not) or
      (has("conclusion") | not) or
      (.conclusion != null and (.conclusion | type) != "string") or
      (.status == "completed" and (.conclusion == null or .conclusion == "")))
    then error("unreported consumer job identity or verdict") else . end |
    if ([.[].id] | unique | length) != length
    then error("duplicate consumer job identity") else . end |
    [.[] | select(.name == "Consumer release rehearsal / Consumer canary")] |
    {count:length, pending:map(select(.status != "completed")) | length,
     jobs:map({name,status,conclusion})}
  ' <<< "$pages")" || return 1
  printf '%s\n' "$verdict"
}

release_require_consumer_verdict() {
  local repository="${1:?repository required}"
  local run_id="${2:?candidate run required}"
  local sha="${3:?candidate commit required}"
  local verdict
  verdict="$(release_consumer_verdict "$repository" "$run_id" "$sha")" || return 1
  echo "Consumer release rehearsal run=$run_id source=$sha verdict=$verdict" >&2
  if ! jq -e '.count == 1 and .pending == 0 and .jobs[0].conclusion == "success"' <<< "$verdict" >/dev/null; then
    echo "::error::Consumer release rehearsal is missing, pending, cancelled or failed; publication refused." >&2
    return 1
  fi
}

# Recover the machine contract of one historical Actions step. This is not a
# new verdict: callers must first authenticate the terminal run and named job.
# Only the runner's env block and this step's records are eligible; matching
# prose, another step, truncated logs and repeated fields all refuse.
release_rehearsal_step_observation() {
  local log="${1:?job log required}" command="${2:?owning command required}"
  local fields="${3:?field names required}"
  jq -Rsec --arg command "$command" --argjson fields "$fields" '
    def exactly_one($label):
      if length == 1 then .[0] else error("missing or duplicate " + $label) end;
    split("\n") | map(
      sub("^\uFEFF"; "") |
      sub("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z "; "") |
      gsub("\u001b\\[[0-9;]*m"; "") |
      gsub("\\\\u001b\\[[0-9;]*m"; "")) |
    . as $lines |
    if (map(select(length > 0)) | last) != "Cleaning up orphan processes"
    then error("incomplete historical job log") else . end |
    [range(0; length) | select($lines[.] == "##[group]Run " + $command)] |
    exactly_one("owning step") as $start |
    $lines[$start + 1:] as $rest |
    ([range(0; $rest | length) | select($rest[.] == "##[endgroup]")] | first) as $end |
    if $end == null then error("incomplete runner step header") else . end |
    $rest[:$end] as $header |
    ([range(0; $header | length) | select($header[.] == "env:")] |
      exactly_one("runner env block")) as $env_start |
    $header[$env_start + 1:] as $env |
    if any($env[]; test("^  [A-Z_][A-Z_0-9]*: ") | not)
    then error("malformed runner env block") else . end |
    [$fields[] as $key |
      ($env | map(select(startswith("  " + $key + ": "))) |
        exactly_one("runner field " + $key)) as $line |
      {key:$key, value:($line | ltrimstr("  " + $key + ": "))}] |
    from_entries as $environment |
    $rest[$end + 1:] |
    ([range(0; length) | select($rest[$end + 1 + .] == "Post job cleanup." or
      ($rest[$end + 1 + .] | startswith("##[group]")))] | first) as $stop |
    if $stop == null then error("missing step termination") else . end |
    {step_start:$start, environment:$environment, records:.[:$stop]}
  ' "$log"
}

# Join the resolver, dispatch, observation and authorization records from one
# failed promotion. Dispatch and observation are distinct steps in the same
# authenticated job; the child's identity must cross that boundary unchanged.
# The API boundary supplies authenticated named jobs separately, so this pure
# parser cannot authorize retirement by itself.
release_failed_rehearsal_observation() {
  local resolver_log="${1:?resolver log required}"
  local consumer_log="${2:?consumer log required}"
  local authorization_log="${3:?authorization log required}"
  local source="${4:?source required}" producer="${5:?producer required}"
  local child="${6:?child run required}" resolver dispatch observation authorization
  resolver="$(release_rehearsal_step_observation "$resolver_log" \
    'bash scripts/resolve-release-promotion-source.sh' \
    '["CANDIDATE_RUN_ID","EXPECTED_SOURCE_SHA"]')" || return 1
  # shellcheck disable=SC2016,SC1003 # literal owning runner command, including its trailing backslash
  dispatch="$(release_rehearsal_step_observation "$consumer_log" \
    'CANARY_REPOSITORY="$CANARY_OWNER/$CANARY_NAME" \' \
    '["SOURCE_REVISION","CANARY_WORKFLOW"]')" || return 1
  # shellcheck disable=SC2016 # literal runner command; do not expand it here
  observation="$(release_rehearsal_step_observation "$consumer_log" \
    'CANARY_REPOSITORY="$CANARY_OWNER/$CANARY_NAME" bash scripts/ci/consumer_canary.sh --observe' \
    '["CANARY_RUN_ID"]')" || return 1
  authorization="$(release_rehearsal_step_observation "$authorization_log" \
    'bash scripts/authorize-release-rehearsal.sh' \
    '["SOURCE_SHA","REQUIRES_REHEARSAL","REHEARSAL_RESULT","REHEARSAL_VERDICT","REHEARSAL_SOURCE_SHA"]')" || return 1
  jq -nce --argjson resolver "$resolver" --argjson dispatch "$dispatch" \
    --argjson observation "$observation" \
    --argjson authorization "$authorization" --arg source "$source" \
    --arg producer "$producer" --arg child "$child" '
    def one_record($records; $prefix; $expected):
      [$records[] | select(startswith($prefix))] as $matches |
      ($matches | length) == 1 and ($matches[0] | test($expected));
    if ($source | test("^[0-9a-f]{40}$")) and
      ($producer | test("^[1-9][0-9]*$")) and ($child | test("^[1-9][0-9]*$")) and
      $resolver.environment == {CANDIDATE_RUN_ID:$producer, EXPECTED_SOURCE_SHA:$source} and
      $dispatch.environment == {SOURCE_REVISION:$source, CANARY_WORKFLOW:"harn-repin-rehearsal.yml"} and
      $observation.environment == {CANARY_RUN_ID:$child} and
      $dispatch.step_start < $observation.step_start and
      $authorization.environment == {SOURCE_SHA:$source, REQUIRES_REHEARSAL:"true",
        REHEARSAL_RESULT:"failure", REHEARSAL_VERDICT:"fail", REHEARSAL_SOURCE_SHA:$source} and
      one_record($dispatch.records; "CONSUMER_CANARY dispatched ";
        "^CONSUMER_CANARY dispatched run=" + $child + " ref=default$") and
      one_record($observation.records; "CONSUMER_CANARY verdict=";
        "^CONSUMER_CANARY verdict=fail conclusion=(failure|cancelled) run=" + $child + " wall_seconds=[0-9]+$") and
      one_record($observation.records; "##[error]Process completed ";
        "^##\\[error\\]Process completed with exit code 1\\.$") and
      one_record($authorization.records; "##[error]Process completed ";
        "^##\\[error\\]Process completed with exit code 1\\.$")
    then {source_sha:$source, producer_run:$producer, consumer_run:$child,
      consumer_conclusion:($observation.records[] | select(startswith("CONSUMER_CANARY verdict=")) |
        capture("conclusion=(?<value>failure|cancelled) ").value),
      verdict:"failed_historical_rehearsal"}
    else error("historical rehearsal machine contracts do not agree") end
  '
}

# Read only the explicitly named historical jobs, never discover authority by
# crawling unrelated runs. Job URLs and run IDs bind every log to its owner.
release_authenticated_failed_rehearsal() (
  set -euo pipefail
  local repository="${1:?repository required}" parent="${2:?promotion run required}"
  local producer="${3:?producer run required}" source="${4:?source required}"
  local resolver="${5:?resolver job required}" consumer="${6:?consumer job required}"
  local authorization="${7:?authorization job required}"
  local child_repository="${8:?consumer repository required}" child="${9:?consumer run required}"
  local failed_job="${10:?failed consumer job required}" run child_run scratch item id name verdict job
  [[ "$repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ &&
     "$child_repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ &&
     "$source" =~ ^[0-9a-f]{40}$ ]] || return 1
  for id in "$parent" "$producer" "$resolver" "$consumer" "$authorization" "$child" "$failed_job"; do
    [[ "$id" =~ ^[1-9][0-9]*$ ]] || return 1
  done
  run="$(gh api "repos/$repository/actions/runs/$parent")"
  jq -e --arg repository "$repository" --arg parent "$parent" '
    (.id | tostring) == $parent and .repository.full_name == $repository and
    .head_repository.full_name == $repository and
    .path == ".github/workflows/promote-release.yml" and
    (.event == "workflow_dispatch" or .event == "workflow_run") and
    .head_branch == "main" and .status == "completed" and .conclusion == "failure" and
    (.run_attempt | type == "number" and . > 0)
  ' <<< "$run" >/dev/null
  child_run="$(gh api "repos/$child_repository/actions/runs/$child")"
  jq -e --arg repository "$child_repository" --arg child "$child" '
    (.id | tostring) == $child and .repository.full_name == $repository and
    .head_repository.full_name == $repository and
    .path == ".github/workflows/harn-repin-rehearsal.yml" and .event == "workflow_dispatch" and
    .status == "completed" and (.conclusion == "failure" or .conclusion == "cancelled")
  ' <<< "$child_run" >/dev/null
  scratch="$(mktemp -d)"
  trap 'rm -rf "$scratch"' EXIT
  for item in \
    "$resolver|Resolve certified source|success|resolver" \
    "$consumer|Recover missing consumer rehearsal / Consumer canary|failure|consumer" \
    "$authorization|Require measured consumer completion|failure|authorization"; do
    IFS='|' read -r id name verdict log <<< "$item"
    job="$(gh api "repos/$repository/actions/jobs/$id")"
    jq -e --arg id "$id" --arg parent "$parent" --arg name "$name" \
      --arg verdict "$verdict" --arg repository "$repository" --argjson run "$run" '
      (.id | tostring) == $id and (.run_id | tostring) == $parent and
      .run_attempt == $run.run_attempt and .name == $name and
      .status == "completed" and .conclusion == $verdict and
      .html_url == ("https://github.com/" + $repository + "/actions/runs/" + $parent + "/job/" + $id)
    ' <<< "$job" >/dev/null
    # Raw runner logs contain ANSI bytes. Store them, then normalize in the
    # parser; never render or evaluate their contents in a terminal.
    gh api --allow-escape-sequences "repos/$repository/actions/jobs/$id/logs" > "$scratch/$log"
  done
  job="$(gh api "repos/$child_repository/actions/jobs/$failed_job")"
  jq -e --arg id "$failed_job" --arg child "$child" --arg repository "$child_repository" \
    --argjson run "$child_run" '
    (.id | tostring) == $id and (.run_id | tostring) == $child and
    .run_attempt == $run.run_attempt and
    .name == "Prove the candidate against the harn-linked TUI suite" and
    .status == "completed" and .conclusion == "failure" and
    .html_url == ("https://github.com/" + $repository + "/actions/runs/" + $child + "/job/" + $id) and
    ([.steps[] | select(.name == "Run the harn-linked TUI suite against the candidate" and
      .status == "completed" and .conclusion == "failure")] | length) == 1
  ' <<< "$job" >/dev/null
  release_failed_rehearsal_observation "$scratch/resolver" "$scratch/consumer" \
    "$scratch/authorization" "$source" "$producer" "$child" |
    jq -ce --arg parent "$parent" --arg failed_job "$failed_job" \
      --argjson child_run "$child_run" \
      'select(.consumer_conclusion == $child_run.conclusion) |
       . + {promotion_run:$parent, failed_consumer_job:$failed_job}'
)
