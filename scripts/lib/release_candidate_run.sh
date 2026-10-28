#!/usr/bin/env bash

# Finds the build run that made a release candidate at one main commit: the
# version commit's push, or a scheduled or requested source candidate. The
# `candidate-manifest-<sha>` artifact is what makes it a candidate rather than
# a warm or skipped build of the same commit.

# Prints the id of a successful "Build release binaries" run that built the
# candidate at <sha>, or nothing when none has. Fails when GitHub cannot be
# read, so a caller never mistakes an unread answer for "no candidate".
release_candidate_run_id() {
  local repository="${1:?repository required}"
  local sha="${2:?commit required}"
  local runs run_id count
  runs="$(gh api \
    "repos/${repository}/actions/workflows/build-release-binaries.yml/runs?head_sha=${sha}&status=success&per_page=30" \
    --jq ".workflow_runs[] | select(.head_sha == \"${sha}\") | .id")" || return 1
  for run_id in $runs; do
    count="$(gh api "repos/${repository}/actions/runs/${run_id}/artifacts?name=candidate-manifest-${sha}" \
      --jq '.total_count')" || return 1
    if [[ "$count" =~ ^[1-9][0-9]*$ ]]; then
      printf '%s\n' "$run_id"
      return 0
    fi
  done
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
  local runs run_id artifacts expected
  expected="$(jq -cer --arg sha "$sha" '
    if type == "array" and length > 0 and all(.[]; .target | type == "string" and length > 0)
    then [.[].target | "harn-" + .] + ["candidate-manifest-" + $sha, "harn-release-files"]
    else error("empty or invalid candidate matrix") end' <<< "$matrix")" || return 1
  runs="$(gh api \
    "repos/${repository}/actions/workflows/build-release-binaries.yml/runs?head_sha=${sha}&status=success&per_page=30" \
    --jq ".workflow_runs[] | select(.head_sha == \"${sha}\" and .head_branch == \"main\") | .id")" || return 1
  for run_id in $runs; do
    artifacts="$(gh api "repos/${repository}/actions/runs/${run_id}/artifacts?per_page=100")" || return 1
    if release_candidate_artifacts_complete "$artifacts" "$expected"; then
      printf '%s\n' "$run_id"
      return 0
    fi
  done
}
