#!/usr/bin/env bash
# Copied from the admitted main workflow before checking out the candidate.
set -euo pipefail
tools="${RUNNER_TEMP:?}/native-qualification-tools"
[[ $(cat "$tools/admitted-source") == "$SOURCE_SHA" && $(git rev-parse HEAD) == "$SOURCE_SHA" ]]
[[ $(cat "$tools/policy-source") == "$GITHUB_SHA" ]]
binary_source=$("${CARGO_TARGET_DIR:-./target}/debug/harn" version --json | jq -er .data.source_revision)
[[ "$binary_source" == "$SOURCE_SHA" ]]
jq -n --arg repo "$GITHUB_REPOSITORY" --arg source "$SOURCE_SHA" --arg policy "$GITHUB_SHA" \
  --argjson run "$GITHUB_RUN_ID" --argjson attempt "$GITHUB_RUN_ATTEMPT" --slurpfile phases "$tools/phases.json" \
  '{kind:"harn.native_qualification.v1",repository:$repo,source_sha:$source,policy_sha:$policy,run_id:$run,run_attempt:$attempt,phases:$phases[0]}' > "$tools/receipt.json"
