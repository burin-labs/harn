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
