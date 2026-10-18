#!/usr/bin/env bash
# Waits for this pull request's CI run to publish its shared Harn CLI and
# prints `run_id=<id>` for the consumer check. A CI run that finishes without
# the artifact, or no artifact before the deadline, fails by name: the consumer
# check never runs against some other build.
#
# Inputs: HEAD_SHA (the pull request head), GH_REPO, GH_TOKEN,
# CLI_WAIT_SECONDS (default 3000, above the CLI build's 49-minute ceiling).
set -euo pipefail

head_sha=${HEAD_SHA:?HEAD_SHA must name the pull request head}
[[ "$head_sha" =~ ^[0-9a-f]{40}$ ]] || { echo "::error::CONSUMER_PR_CHECK reason=head_invalid"; exit 1; }
deadline=$(($(date +%s) + ${CLI_WAIT_SECONDS:-3000}))

while true; do
  run=$(gh api "repos/$GH_REPO/actions/workflows/ci.yml/runs?event=pull_request&head_sha=$head_sha&per_page=1" \
    --jq '.workflow_runs[0] | "\(.id) \(.status)"' 2> /dev/null || true)
  read -r run_id status <<< "$run"
  if [[ "${run_id:-}" =~ ^[0-9]+$ ]]; then
    artifact=$(gh api "repos/$GH_REPO/actions/runs/$run_id/artifacts?name=harn-cli.tar.zst" \
      --jq '[.artifacts[] | select(.expired == false)] | length' 2> /dev/null || echo 0)
    if [[ "$artifact" =~ ^[1-9] ]]; then
      echo "CONSUMER_PR_CHECK cli_run=$run_id" >&2
      echo "run_id=$run_id"
      exit 0
    fi
    if [[ "$status" == completed ]]; then
      echo "::error::CONSUMER_PR_CHECK reason=ci_completed_without_cli run=$run_id"
      exit 1
    fi
  fi
  if (($(date +%s) >= deadline)); then
    echo "::error::CONSUMER_PR_CHECK reason=cli_not_published run=${run_id:-none}"
    exit 1
  fi
  sleep 30
done
