#!/usr/bin/env bash

# Finds the build run that made a release candidate. A release is built in its
# merge group, or by the push to main when no queue run built it; either run
# uploads `candidate-manifest-<sha>`, which is what makes it a candidate rather
# than a warm or skipped build of the same commit.

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
    if jq -e --argjson expected "$expected" '
      . as $response |
      (.artifacts | type == "array") and
      (.total_count == (.artifacts | length)) and
      all($expected[]; . as $name |
        [$response.artifacts[] | select(.name == $name and .expired == false and .size_in_bytes > 0)] | length == 1)
    ' <<< "$artifacts" >/dev/null; then
      printf '%s\n' "$run_id"
      return 0
    fi
  done
}

# Resolves an exact-SHA merge-group candidate for the push workflow. A release
# can reach main while its candidate is still running, so a success-only read
# makes "still building" indistinguishable from "absent" and starts a duplicate
# build. This resolver preserves that state and waits a bounded number of
# times. It prints one machine-readable line:
#
#   success|RUN_ID|
#   absent||
#   failed|RUN_ID|CONCLUSION
#   missing_manifest|RUN_ID|
#   timed_out|RUN_ID|STATUS
#
# GitHub read failures return nonzero so callers cannot confuse an unread
# result with absence. The current push run is excluded explicitly because it
# has the same head SHA and is necessarily in progress during setup.
release_candidate_run_resolution() {
  local repository="${1:?repository required}"
  local sha="${2:?commit required}"
  local excluded_run_id="${3:-}"
  local max_attempts="${4:-60}"
  local poll_seconds="${5:-60}"
  local attempt runs rows run_id status conclusion count
  local in_flight_id in_flight_status terminal_state terminal_id terminal_detail

  [[ "$max_attempts" =~ ^[1-9][0-9]*$ ]] || return 1
  [[ "$poll_seconds" =~ ^[0-9]+$ ]] || return 1

  for ((attempt = 1; attempt <= max_attempts; attempt++)); do
    runs="$(gh api \
      "repos/${repository}/actions/workflows/build-release-binaries.yml/runs?head_sha=${sha}&per_page=30")" \
      || return 1
    rows="$(jq -r \
      --arg sha "$sha" \
      --arg excluded "$excluded_run_id" \
      '.workflow_runs[]
       | select(.head_sha == $sha and .event == "merge_group")
       | select((.id | tostring) != $excluded)
       | [.id, .status, (.conclusion // "")] | @tsv' \
      <<< "$runs")" || return 1

    in_flight_id=""
    in_flight_status=""
    terminal_state=""
    terminal_id=""
    terminal_detail=""
    while IFS=$'\t' read -r run_id status conclusion; do
      [[ -n "$run_id" ]] || continue
      if [[ "$status" == completed && "$conclusion" == success ]]; then
        count="$(gh api \
          "repos/${repository}/actions/runs/${run_id}/artifacts?name=candidate-manifest-${sha}" \
          --jq '.total_count')" || return 1
        if [[ "$count" =~ ^[1-9][0-9]*$ ]]; then
          printf 'success|%s|\n' "$run_id"
          return 0
        fi
        terminal_state=missing_manifest
        terminal_id="$run_id"
      elif [[ "$status" != completed ]]; then
        if [[ -z "$in_flight_id" ]]; then
          in_flight_id="$run_id"
          in_flight_status="$status"
        fi
      elif [[ -z "$terminal_state" ]]; then
        terminal_state=failed
        terminal_id="$run_id"
        terminal_detail="${conclusion:-unknown}"
      fi
    done <<< "$rows"

    if [[ -n "$in_flight_id" ]]; then
      if ((attempt == max_attempts)); then
        printf 'timed_out|%s|%s\n' "$in_flight_id" "$in_flight_status"
        return 0
      fi
      echo "::notice::Exact-SHA merge-group candidate run $in_flight_id is $in_flight_status; waiting before read $((attempt + 1))/$max_attempts." >&2
      ((poll_seconds == 0)) || sleep "$poll_seconds"
      continue
    fi
    if [[ -n "$terminal_state" ]]; then
      printf '%s|%s|%s\n' "$terminal_state" "$terminal_id" "$terminal_detail"
      return 0
    fi
    printf 'absent||\n'
    return 0
  done
}
