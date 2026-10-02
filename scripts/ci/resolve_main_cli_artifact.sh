#!/usr/bin/env bash
# Find the newest main commit whose CI run published the shared Harn CLI, and
# write `sha`, `run_id` and `artifact_id` to the given GitHub output file.
#
# The CLI artifact is produced by whichever CI run proved a commit: the
# merge-queue run for a queued pull request, or the push run when main pushes
# still run the heavy lanes. Both carry the same commit SHA, so the question is
# asked per commit rather than per branch name or event, which keeps this
# reader independent of how pushes are routed.
#
# Absence is a failure, never an empty success: when no commit in the window
# has an unexpired CLI, this exits non-zero and says so.
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <github-output-file>" >&2
  exit 2
fi
output=$1

: "${GH_REPO:?GH_REPO must be set}"
: "${GH_TOKEN:?GH_TOKEN must be set}"

readonly ARTIFACT_NAME="harn-cli.tar.zst"
readonly BRANCH="${HARN_CLI_ARTIFACT_BRANCH:-main}"
readonly COMMIT_WINDOW="${HARN_CLI_ARTIFACT_COMMIT_WINDOW:-30}"

commits="$(gh api "repos/${GH_REPO}/commits?sha=${BRANCH}&per_page=${COMMIT_WINDOW}" --jq '.[].sha')"
while IFS= read -r sha; do
  [[ "$sha" =~ ^[0-9a-f]{40}$ ]] || continue
  runs="$(gh api "repos/${GH_REPO}/actions/workflows/ci.yml/runs?head_sha=${sha}&per_page=20" \
    --jq '.workflow_runs[].id')"
  while IFS= read -r run_id; do
    [[ -n "$run_id" ]] || continue
    artifact_id="$(gh api "repos/${GH_REPO}/actions/runs/${run_id}/artifacts?per_page=100" \
      --jq "[.artifacts[] | select(.name == \"${ARTIFACT_NAME}\" and .expired == false) | .id] | .[0] // empty")"
    if [[ -n "$artifact_id" ]]; then
      {
        echo "sha=$sha"
        echo "run_id=$run_id"
        echo "artifact_id=$artifact_id"
      } >> "$output"
      echo "Shared Harn CLI for ${sha}: run ${run_id}, artifact ${artifact_id}"
      exit 0
    fi
  done <<< "$runs"
done <<< "$commits"

echo "::error::No unexpired ${ARTIFACT_NAME} on the newest ${COMMIT_WINDOW} ${BRANCH} commits." >&2
exit 1
