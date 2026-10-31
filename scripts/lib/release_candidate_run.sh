#!/usr/bin/env bash

# Finds the build run that made a release candidate at one main commit: the
# version commit's push, or a scheduled or requested source candidate. The
# `candidate-manifest-<sha>` artifact is what makes it a candidate rather than
# a warm or skipped build of the same commit.

# Return 0 for a source dispatch, 1 for a non-candidate dispatch, and 2 when
# the purpose cannot be established. New runs expose their input in their
# immutable run name; older runs expose it in the setup job's conditional step.
release_dispatch_is_candidate() {
  local repository="${1:?repository required}" run_id="${2:?run required}" title="${3:-}"
  local decision response
  case "$title" in
    'Build release binaries [source-candidate]') return 0 ;;
    'Build release binaries [benchmark]'|'Build release binaries [warm]'|'Build release binaries [invalid]') return 1 ;;
    'Build release binaries') ;;
    *) return 2 ;;
  esac
  response="$(gh api "repos/${repository}/actions/runs/${run_id}/jobs?per_page=100" --paginate --slurp < /dev/null)" || return 2
  decision="$(jq -r '
    if type != "array" or length == 0 or (all(.[]; (.jobs | type) == "array") | not) or (.[0].total_count | type) != "number" or ([.[].jobs | length] | add) != .[0].total_count then error("incomplete jobs census") else . end |
    [.[].jobs[] | select(.name == "Resolve release context")] as $jobs |
    if ($jobs | length) != 1 then "unknown"
    else $jobs[0].steps as $steps |
      [$steps[] | select(.name | startswith("Run actions/checkout@"))] as $checkout |
      [$steps[] | select(.name == "Install the source candidate decision runtime")] as $install |
      if ($checkout | length) != 1 or $checkout[0].conclusion != "success" or ($install | length) != 1 then "unknown"
      elif $install[0].conclusion == "skipped" then "other"
      elif $install[0].conclusion == "success" or $install[0].conclusion == "failure" then "candidate"
      else "unknown" end
    end' <<< "$response")" || return 2
  case "$decision" in
    candidate) return 0 ;;
    other) return 1 ;;
    *) return 2 ;;
  esac
}

# Prints the id of a successful "Build release binaries" run that built the
# candidate at <sha>, or nothing when none has. Fails when GitHub cannot be
# read, so a caller never mistakes an unread answer for "no candidate".
release_candidate_run_id() {
  local repository="${1:?repository required}"
  local sha="${2:?commit required}"
  local reader
  reader="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/release-candidate-state.mjs"
  node "$reader" successful "$repository" "$sha"
}

# Validate one measured artifact inventory before source reuse or recovery.
release_candidate_artifacts_complete() {
  local artifacts="${1:?artifact response required}"
  local expected="${2:?expected artifact names required}"
  jq -e --argjson expected "$expected" '
    . as $response |
    (.artifacts | type == "array") and
    (.total_count == (.artifacts | length)) and
    all($expected[]; . as $name |
      [$response.artifacts[] | select(.name == $name)] as $matches |
      ($matches | length) == 1 and
      all($matches[]; .expired == false and
        (.size_in_bytes | type == "number" and . > 0 and . == floor)))
  ' <<< "$artifacts" >/dev/null
}

# A scheduled source build may reuse only a complete, unexpired artifact set.
# Target names come from the producer's resolved matrix, not a parallel list.
source_candidate_live_run_id() {
  local repository="${1:?repository required}"
  local sha="${2:?commit required}"
  local matrix="${3:?build matrix required}"
  local runs run_id artifacts expected reader
  expected="$(jq -cer --arg sha "$sha" '
    if type == "array" and length > 0 and all(.[]; .target | type == "string" and length > 0)
    then [.[].target | "harn-" + .] + ["candidate-manifest-" + $sha, "harn-release-files"]
    else error("empty or invalid candidate matrix") end' <<< "$matrix")" || return 1
  reader="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/release-candidate-state.mjs"
  runs="$(node "$reader" successful-main-runs "$repository" "$sha")" || return 1
  for run_id in $runs; do
    artifacts="$(node "$reader" artifacts "$repository" "$run_id")" || return 1
    if release_candidate_artifacts_complete "$artifacts" "$expected"; then
      printf '%s\n' "$run_id"
      return 0
    fi
  done
}
