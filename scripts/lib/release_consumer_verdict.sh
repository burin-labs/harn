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
       any(.[]; (.jobs | type) != "array" or (.total_count | type) != "number")
    then error("missing consumer job census") else . end |
    . as $pages | [.[].jobs[]] as $jobs |
    if all($pages[]; .total_count == ($jobs | length)) then $jobs
    else error("partial consumer job census") end |
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
