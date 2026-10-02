#!/usr/bin/env bash
#
# Carry the `main health` status from the commit a push replaced to the pushed
# commit, or decline so .github/workflows/main-health.yml measures instead.
#
# It declines (writes carried=false to GITHUB_OUTPUT) when:
#   - there is no replaced commit (first or force push) or it has no status;
#   - the push changed what main health watches or how it judges, since a
#     verdict made under the old rules must not stand for the new ones;
#   - the push's changed files cannot be listed, or the list reached GitHub's
#     300-file cap and may be incomplete.
#
# Environment: GH_REPO, PUSHED_SHA, REPLACED_SHA, GITHUB_OUTPUT,
# GITHUB_STEP_SUMMARY.
set -euo pipefail

# The files that define what is watched and how it is judged.
policy_paths=(
  .github/workflows/main-health.yml
  scripts/ci/main_health.sh
  scripts/ci/main_health_carry.sh
  scripts/check_scheduled_workflows.harn
  scripts/scheduled_workflows.toml
)

decline() {
  echo "carried=false" >> "$GITHUB_OUTPUT"
  echo "Measuring instead of carrying: $1." >> "$GITHUB_STEP_SUMMARY"
  exit 0
}

if [[ -z "${REPLACED_SHA:-}" || "$REPLACED_SHA" == 0000000000000000000000000000000000000000 ]]; then
  decline "no replaced commit"
fi

if ! changed="$(gh api "repos/$GH_REPO/compare/$REPLACED_SHA...$PUSHED_SHA" \
  --jq '(.files | length | "count \(.)"), (.files[].filename)')"; then
  decline "the push's changed files could not be listed"
fi
count="$(head -n 1 <<< "$changed")"
if [[ ! "$count" =~ ^count\ [0-9]+$ ]] || (( ${count#count } >= 300 )); then
  decline "the push's changed-file list is incomplete"
fi
if grep -Fxq -f <(printf '%s\n' "${policy_paths[@]}") <(tail -n +2 <<< "$changed"); then
  decline "the push changed the health policy or reader"
fi

previous="$(gh api "repos/$GH_REPO/commits/$REPLACED_SHA/statuses" \
  --jq '[.[] | select(.context == "main health")][0] // empty | [.state, .description, (.target_url // "")] | @tsv')" || previous=""
if [[ -z "$previous" ]]; then
  decline "$REPLACED_SHA has no main health status"
fi

IFS=$'\t' read -r state description target <<< "$previous"
gh api "repos/$GH_REPO/statuses/$PUSHED_SHA" -X POST \
  -f state="$state" -f context="main health" -f description="$description" \
  ${target:+-f target_url="$target"} >/dev/null
echo "carried=true" >> "$GITHUB_OUTPUT"
echo "Carried \`$state: $description\` forward from $REPLACED_SHA." >> "$GITHUB_STEP_SUMMARY"
