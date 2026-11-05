#!/usr/bin/env bash
#
# Advance the protected `green` branch to the newest main commit whose full CI
# suite passed, and record each advance as an annotated `green-*` tag.
#
# Every push to main gets its own never-cancelled CI run that selects every
# proof family (ci.yml's concurrency key and scripts/ci/post_merge_tier.sh), so
# a successful one certifies its commit and no extra run is needed.
#
# A pass reads every successful main push run created in the last
# GREEN_CENSUS_DAYS days, newest first. A run whose commit is still on main and
# strictly ahead of green gets a tag `green-<created UTC>-<run id>` whose message
# ends with one JSON record, then green fast-forwards to it. An existing tag
# must name the same run. Runs arrive in main's push order, so the pass stops
# at the first run that advances green or is already contained in it. A failed
# run does not stop older ones; the pass fails at the end if any run failed. An incomplete or unreadable census fails
# the pass before any write.
#
# A pass can also keep the full release build exercised. With
# GREEN_SOURCE_CANDIDATE_HOURS set, it dispatches a signed source candidate of
# main's tip when none is running, the tip has none, and the last one started
# at least that long ago. Unset starts nothing. With
# GREEN_REQUIRES_SOURCE_CANDIDATE=true, a run moves green only when its commit
# has a successful candidate; otherwise older runs are considered.
#
# Environment: GH_TOKEN (contents and workflows write, actions write to start
# candidates), GITHUB_REPOSITORY, GREEN_CENSUS_DAYS (default 3),
# GREEN_SOURCE_CANDIDATE_HOURS, GREEN_REQUIRES_SOURCE_CANDIDATE,
# GITHUB_STEP_SUMMARY (optional).
set -euo pipefail

scripts_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/green_checkpoint.sh
source "$scripts_dir/lib/green_checkpoint.sh"
# shellcheck source=scripts/lib/release_candidate_run.sh
source "$scripts_dir/lib/release_candidate_run.sh"

repo=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}
days=${GREEN_CENSUS_DAYS:-3}
summary=${GITHUB_STEP_SUMMARY:-/dev/null}
candidate_hours=${GREEN_SOURCE_CANDIDATE_HOURS:-}
require_candidate=${GREEN_REQUIRES_SOURCE_CANDIDATE:-}
[[ "$days" =~ ^[1-9][0-9]*$ ]] || { echo "::error::GREEN_CENSUS_DAYS must be a positive integer" >&2; exit 1; }
# A cadence longer than the census would lose sight of the last candidate and start one every pass.
if [[ -n "$candidate_hours" ]] && { [[ ! "$candidate_hours" =~ ^[1-9][0-9]*$ ]] || (( candidate_hours > days * 24 )); }; then
  echo "::error::GREEN_SOURCE_CANDIDATE_HOURS must be a positive integer of at most $((days * 24)) hours" >&2
  exit 1
fi
# A misspelling must not read as off, which would let green pass an unbuilt commit.
case "$require_candidate" in
  "" | true | false) ;;
  *) echo "::error::GREEN_REQUIRES_SOURCE_CANDIDATE must be true or false" >&2; exit 1 ;;
esac

refuse() {
  echo "::error title=Green checkpoint::$1" >&2
  echo "Green checkpoint refused: $1" >> "$summary"
  exit 1
}

since=$(jq -nr --argjson days "$days" 'now - $days * 86400 | strftime("%Y-%m-%d")')
census=$(gh api --paginate --slurp \
  "repos/$repo/actions/workflows/ci.yml/runs?branch=main&event=push&status=success&created=%3E%3D$since&per_page=100") \
  || refuse "the CI run census could not be read"
# Every page must agree on one total and the pages must add up to it; a short
# read would otherwise look like a quiet main.
if ! candidates=$(jq -r --arg repo "$repo" '
  if type != "array" or length == 0
    or any(.[]; (.total_count | type) != "number" or (.workflow_runs | type) != "array")
  then error("malformed census") else . end
  | if (map(.total_count) | unique | length) != 1
      or (map(.workflow_runs | length) | add) != .[0].total_count
    then error("incomplete census") else . end
  | [.[].workflow_runs[]
      | select(.path == ".github/workflows/ci.yml" and .event == "push"
          and .head_branch == "main" and .status == "completed"
          and .conclusion == "success" and .head_repository.full_name == $repo)]
  | if any(.[]; (.id | type) != "number" or (.run_attempt | type) != "number"
        or (.head_sha | type != "string" or (test("^[0-9a-f]{40}$") | not))
        or (.created_at | type != "string" or (test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$") | not))
        or (.updated_at | type) != "string" or (.html_url | type) != "string")
    then error("malformed run") else . end
  | sort_by(.created_at) | reverse | .[]
  | [.id, .run_attempt, .head_sha, .html_url, .created_at, .updated_at] | @tsv
' <<< "$census"); then
  refuse "the CI run census was incomplete or malformed"
fi

green=$(green_tip "$repo") || refuse "the green branch could not be read"
initial=$green

# Create the checkpoint tag, or verify an existing one names the same run.
write_tag() {
  local tag=$1 sha=$2 run_id=$3 attempt=$4 url=$5 completed=$6
  local existing object record payload
  existing=$(gh api "repos/$repo/git/matching-refs/tags/$tag") || return 1
  object=$(jq -r --arg ref "refs/tags/$tag" '
    [.[] | select(.ref == $ref)]
    | if length == 0 then ""
      elif .[0].object.type == "tag" then .[0].object.sha
      else error("lightweight tag") end' <<< "$existing") || return 1
  if [[ -n "$object" ]]; then
    gh api "repos/$repo/git/tags/$object" | jq -e --arg sha "$sha" --argjson run "$run_id" \
      --arg schema "$GREEN_CHECKPOINT_SCHEMA" '
      .object.sha == $sha
      and (.message | split("\n") | map(select(length > 0)) | last | fromjson
        | .schema == $schema and .sha == $sha and .runId == $run)' > /dev/null || {
      echo "::error::$tag exists but does not record run $run_id at $sha" >&2
      return 1
    }
    return 0
  fi
  record=$(jq -cn --arg schema "$GREEN_CHECKPOINT_SCHEMA" --arg sha "$sha" \
    --argjson run "$run_id" --argjson attempt "$attempt" --arg url "$url" --arg completed "$completed" \
    '{schema: $schema, sha: $sha, runId: $run, runAttempt: $attempt, runUrl: $url, completedAt: $completed}')
  payload=$(jq -n --arg tag "$tag" --arg sha "$sha" \
    --arg message "Green checkpoint: CI run $run_id passed the full suite at $sha."$'\n\n'"$record" \
    '{tag: $tag, message: $message, object: $sha, type: "commit"}')
  object=$(gh api -X POST "repos/$repo/git/tags" --input - --jq '.sha' <<< "$payload") || return 1
  [[ "$object" =~ ^[0-9a-f]{40}$ ]] || return 1
  gh api -X POST "repos/$repo/git/refs" -f "ref=refs/tags/$tag" -f "sha=$object" > /dev/null
}

# Move green to SHA without ever rewinding it, and read the answer back.
advance() {
  local sha=$1 moved
  if [[ -z "$green" ]]; then
    moved=$(gh api -X POST "repos/$repo/git/refs" -f ref=refs/heads/green -f "sha=$sha" --jq '.object.sha') || return 1
  else
    moved=$(gh api -X PATCH "repos/$repo/git/refs/heads/green" -f "sha=$sha" -F force=false --jq '.object.sha') || return 1
  fi
  [[ "$moved" == "$sha" ]]
}

settle_run() {
  local run_id=$1 attempt=$2 sha=$3 url=$4 created=$5 completed=$6 relation on_main tag candidate
  if [[ -n "$green" ]]; then
    relation=$(green_compare "$repo" "$green" "$sha") || { echo "::error::could not compare $sha with green" >&2; return 1; }
    if [[ "$relation" == behind || "$relation" == identical ]]; then
      settled=true
      return 0
    fi
  fi
  on_main=$(green_compare "$repo" "$sha" main) || { echo "::error::could not compare $sha with main" >&2; return 1; }
  if [[ "$on_main" != ahead && "$on_main" != identical ]]; then
    echo "::notice::Run $run_id certified $sha, which is no longer on main; skipped."
    return 0
  fi
  # Two main commits off one line means main's history was rewritten under green.
  if [[ "${relation:-ahead}" == diverged ]]; then
    echo "::error::$sha is on main but diverged from green $green" >&2
    return 1
  fi
  if [[ "$require_candidate" == true ]]; then
    candidate=$(release_candidate_run_id "$repo" "$sha") || { echo "::error::could not read the release candidates at $sha" >&2; return 1; }
    if [[ -z "$candidate" ]]; then
      held+=("$sha")
      return 0
    fi
  fi
  tag="green-$(tr -d ':-' <<< "$created")-$run_id"
  write_tag "$tag" "$sha" "$run_id" "$attempt" "$url" "$completed" || { echo "::error::could not record $tag" >&2; return 1; }
  advance "$sha" || { echo "::error::could not fast-forward green to $sha" >&2; return 1; }
  echo "Advanced green to $sha ($tag)."
  green=$sha
  settled=true
}

# Dispatch a source candidate of main's tip when one is due, and say why not otherwise.
start_source_candidate() {
  local runs tip decision
  if [[ -z "$candidate_hours" ]]; then
    echo "Source candidates: not started by this workflow (GREEN_SOURCE_CANDIDATE_HOURS is unset)."
    return 0
  fi
  runs=$(gh api --paginate --slurp \
    "repos/$repo/actions/workflows/build-release-binaries.yml/runs?branch=main&created=%3E%3D$since&per_page=100") || return 1
  tip=$(gh api "repos/$repo/git/ref/heads/main" --jq '.object.sha') || return 1
  [[ "$tip" =~ ^[0-9a-f]{40}$ ]] || return 1
  decision=$(jq -r --arg tip "$tip" --argjson hours "$candidate_hours" '
    if type != "array" or length == 0
      or any(.[]; (.total_count | type) != "number" or (.workflow_runs | type) != "array")
      or (map(.total_count) | unique | length) != 1
      or (map(.workflow_runs | length) | add) != .[0].total_count
    then error("incomplete census") else . end
    | [.[].workflow_runs[] | select(.display_title == "Build release binaries [source-candidate]")] as $candidates
    | ($candidates | map(select(.status != "completed")) | length) as $active
    | if $active > 0 then "skip: \($active) still running"
      elif any($candidates[]; .head_sha == $tip) then "skip: main'"'"'s tip already has one"
      elif ($candidates | length) > 0 and (now - ($candidates | map(.created_at | fromdate) | max)) < $hours * 3600
      then "skip: the last one started under \($hours) h ago"
      else "start" end' <<< "$runs") || return 1
  echo "Source candidates: $decision"
  [[ "$decision" == start ]] || return 0
  jq -n '{ref: "main", inputs: {source_candidate: "true"}}' \
    | gh api -X POST "repos/$repo/actions/workflows/build-release-binaries.yml/dispatches" --input - > /dev/null || return 1
  echo "Source candidates: dispatched at $tip."
}

failed=()
held=()
count=0
settled=false
while IFS=$'\t' read -r run_id attempt sha url created completed; do
  [[ -n "$run_id" ]] || continue
  count=$((count + 1))
  if ! settle_run "$run_id" "$attempt" "$sha" "$url" "$created" "$completed"; then
    failed+=("run $run_id at $sha")
  fi
  [[ "$settled" == false ]] || break
done <<< "$candidates"

start_source_candidate | tee -a "$summary" || failed+=("starting a source candidate")

{
  echo "Green checkpoint: read $count of the successful main runs since $since."
  echo "green: ${initial:-absent} -> ${green:-absent}"
  echo "requires a release candidate: ${require_candidate:-false}; held commits: ${#held[@]} ${held[*]:-}"
} | tee -a "$summary"
if (( ${#failed[@]} > 0 )); then
  printf '::error title=Green checkpoint::%s failed\n' "${failed[@]}" >&2
  printf 'failed: %s\n' "${failed[@]}" >> "$summary"
  exit 1
fi
