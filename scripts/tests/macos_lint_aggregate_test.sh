#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
fixture="$scratch/checkout"
mkdir -p "$fixture/scripts/ci"
cp "$root/scripts/ci/require_macos_lint.sh" "$root/scripts/ci/run_rust_lint_lane.sh" "$fixture/scripts/ci/"
git init -q --object-format=sha1 "$fixture"
git -C "$fixture" add scripts
git -C "$fixture" -c core.hooksPath=/dev/null -c commit.gpgSign=false \
  -c user.name=Fixture -c user.email=fixture@example.invalid commit -qm fixture
guard="$fixture/scripts/ci/require_macos_lint.sh"
export GITHUB_REPOSITORY=burin-labs/fixture GITHUB_RUN_ID=71 GITHUB_RUN_ATTEMPT=1
export GITHUB_SHA=$(git -C "$fixture" rev-parse HEAD)
export GITHUB_WORKFLOW_REF=burin-labs/fixture/.github/workflows/ci.yml@refs/heads/main
export GITHUB_OUTPUT="$scratch/outputs" GITHUB_JOB=macos-lint LINT_OUTCOME=success
while IFS= read -r leg; do
  LINT_LEG="$leg" bash "$guard" record > "$scratch/record-log"
done < <(bash "$fixture/scripts/ci/run_rust_lint_lane.sh" --list-legs)
jq -Rsc 'split("\n") | map(select(length>0) | capture("^(?<key>[^=]+)=(?<value>.*)$")) | from_entries' \
  "$GITHUB_OUTPUT" > "$scratch/complete.json"
export LINT_PROOFS=$(cat "$scratch/complete.json") MATRIX_RESULT=success GITHUB_JOB=macos
bash "$guard" > "$scratch/log"
grep -q 'expected=3 observed=3 pending=0 bad=0' "$scratch/log"
tail -n 1 "$scratch/log" | jq -e '.decision=="passed" and .consumerAttempt==1 and (.roles|length)==3' > /dev/null
export GITHUB_RUN_ATTEMPT=2
bash "$guard" > "$scratch/log"
tail -n 1 "$scratch/log" | jq -e '.consumerAttempt==2 and all(.roles[];.receipt.producerAttempt==1)' > /dev/null
echo 'PASS: distinct nonempty role receipts preserve producer identity across consumer attempts'

refuse() {
  if bash "$guard" > "$scratch/log" 2>&1; then
    echo "Unqualified macOS proof accepted: $1" >&2
    exit 1
  fi
}
for mutation in \
  '{}' 'null' 'del(.workspace)' '.workspace=""' '.workspace="not-json"' \
  '.workspace=."lean-lsp"' '.unexpected=.workspace' \
  '.workspace|= (fromjson | .source="2222222222222222222222222222222222222222" | tojson)' \
  '.workspace|= (fromjson | .runId="72" | tojson)' \
  '.workspace|= (fromjson | .repository="foreign/repo" | tojson)' \
  '.workspace|= (fromjson | .workflowRef="foreign-workflow" | tojson)' \
  '.workspace|= (fromjson | .producerAttempt=3 | tojson)' \
  '.workspace|= (fromjson | .producerAttempt=0 | tojson)' \
  '.workspace|= (fromjson | del(.producerAttempt) | tojson)' \
  '.workspace|= (fromjson | .conclusion="failure" | tojson)' \
  '.workspace|= (fromjson | .conclusion="skipped" | tojson)'; do
  jq "$mutation" "$scratch/complete.json" > "$scratch/invalid.json"
  LINT_PROOFS=$(cat "$scratch/invalid.json") refuse "$mutation"
done
for result in failure skipped cancelled unreported; do
  MATRIX_RESULT="$result" refuse "$result matrix"
done
echo 'PASS: empty, partial, duplicated, malformed, foreign, future and failed evidence refuses'

bytes=$(wc -c < "$GITHUB_OUTPUT")
for result in failure skipped unreported; do
  if GITHUB_JOB=macos-lint LINT_LEG=workspace LINT_OUTCOME="$result" bash "$guard" record > "$scratch/log" 2>&1; then
    echo "Unreached lint emitted a receipt: $result" >&2
    exit 1
  fi
done
if GITHUB_JOB=macos-lint LINT_LEG=all bash "$guard" record > "$scratch/log" 2>&1; then
  echo 'Unidentified lint role emitted a receipt' >&2
  exit 1
fi
[[ $(wc -c < "$GITHUB_OUTPUT") == "$bytes" ]]
printf '\n# changed source\n' >> "$fixture/scripts/ci/run_rust_lint_lane.sh"
refuse changed-checkout
echo 'PASS: only reached successful named lint can emit; changed checkout refuses'
echo 'macos_lint_aggregate_test: ok'
