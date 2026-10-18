#!/usr/bin/env bash
# This runs from the trusted workflow revision before any candidate execution.
set -euo pipefail
[[ ${GITHUB_EVENT_NAME:-} == workflow_dispatch && ${GITHUB_REF:-} == refs/heads/main ]]
[[ ${SOURCE_SHA:-} =~ ^[0-9a-f]{40}$ && ${GITHUB_SHA:-} =~ ^[0-9a-f]{40}$ ]]
[[ $(git rev-parse HEAD) == "$GITHUB_SHA" ]]
git merge-base --is-ancestor "$SOURCE_SHA" "$GITHUB_SHA"
# The CLI records this compile-time identity; Cargo rebuilds when it changes.
printf 'HARN_BUILD_REVISION=%s\n' "$SOURCE_SHA" >> "${GITHUB_ENV:?}"
tools="${RUNNER_TEMP:?}/native-qualification-tools"
mkdir -p "$tools"
cp scripts/ci/native_qualification_probe.sh "$tools/probe.sh"
cp scripts/ci/native_qualification_receipt.sh "$tools/receipt.sh"
jq '.platforms[] | select(.name == "macos") | .required_steps' scripts/release_audit_contract.json > "$tools/phases.json"
printf '%s\n' "$SOURCE_SHA" > "$tools/admitted-source"
printf '%s\n' "$GITHUB_SHA" > "$tools/policy-source"
