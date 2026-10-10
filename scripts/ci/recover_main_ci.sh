#!/usr/bin/env bash
set -euo pipefail
: "${GH_REPO:?repository required}"
: "${RUN_ID:?run required}"
[[ "$RUN_ID" =~ ^[0-9]+$ ]] || exit 1
run=$(gh api "repos/$GH_REPO/actions/runs/$RUN_ID")
jq -e --arg repo "$GH_REPO" '.event == "push" and .head_branch == "main" and .head_repository.full_name == $repo and .name == "CI" and .status == "completed"' <<< "$run" >/dev/null
sha=$(jq -er '.head_sha' <<< "$run")
attempt=$(jq -er '.run_attempt' <<< "$run")
conclusion=$(jq -er '.conclusion' <<< "$run")
[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || exit 1
[[ "$attempt" =~ ^[1-9][0-9]*$ ]] || exit 1
if [[ "$conclusion" == success && "$attempt" == 1 ]]; then exit 0; fi
if [[ "$conclusion" != failure && "$conclusion" != success ]]; then
  echo "No automatic action for $conclusion."
  exit 0
fi
if [[ "$conclusion" == failure && "$attempt" == 1 ]]; then
  gh run rerun "$RUN_ID" --repo "$GH_REPO" --failed
  exit 0
fi

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
marker="<!-- main-ci-recovery:$RUN_ID -->"
existing=$(gh issue list --repo "$GH_REPO" --state all --search "$marker in:body" --json number --jq '.[0].number // empty')
if [[ -n "$existing" ]]; then
  echo "Run already recorded in issue #$existing."
  exit 0
fi
jobs=$(gh api --paginate "repos/$GH_REPO/actions/runs/$RUN_ID/attempts/1/jobs?per_page=100" --jq '.jobs[] | select(.conclusion == "failure" and .name != "CI status") | .name')
[[ -n "$jobs" ]] || { echo 'No measured first-attempt failures; refusing classification.' >&2; exit 1; }
# shellcheck disable=SC2016 # Markdown code spans, not shell substitutions.
printf '%s\n\nSource: `%s`\nRun: https://github.com/%s/actions/runs/%s\n\nFirst-attempt failing jobs:\n%s\n' "$marker" "$sha" "$GH_REPO" "$RUN_ID" "$jobs" > "$scratch/body"
if [[ "$conclusion" == success ]]; then
  gh issue create --repo "$GH_REPO" --title "[CI] Flaky main suite at ${sha:0:12}" --body-file "$scratch/body"
  exit 0
fi
latest=$(gh api --paginate "repos/$GH_REPO/actions/runs/$RUN_ID/jobs?filter=latest&per_page=100" --jq '.jobs[] | select(.conclusion == "failure" and .name != "CI status") | .name')
[[ -n "$latest" ]] || { echo 'No measured retry failures; refusing culprit attribution.' >&2; exit 1; }
# Repeated jobs, even a test step, can fail during setup or compilation. These
# observations do not establish a source culprit. Record the complete census
# for diagnosis; source changes go through an independently reviewed PR.
gh api --paginate --slurp "repos/$GH_REPO/actions/runs/$RUN_ID/attempts/1/jobs?per_page=100" > "$scratch/first-pages.json"
gh api --paginate --slurp "repos/$GH_REPO/actions/runs/$RUN_ID/jobs?filter=latest&per_page=100" > "$scratch/latest-pages.json"
jq -n --slurpfile first "$scratch/first-pages.json" --slurpfile latest "$scratch/latest-pages.json" '
  def census:
    if type != "array" or length == 0 then error("missing job pages") else . end
    | . as $pages | [$pages[] | .jobs[]] as $jobs
    | if ($pages[0].total_count | type) != "number"
         or ($pages[0].total_count != ($jobs | length))
         or any($pages[]; .total_count != $pages[0].total_count)
      then error("incomplete job census") else $jobs end;
  def evidence:
    . as $jobs | {
      measured_jobs: length,
      pending: [$jobs[] | select(.status != "completed") | .name],
      failing: [$jobs[] | select(.conclusion == "failure") | {
        name,
        failed_steps: [.steps[]? | select(.conclusion == "failure") | .name],
        steps_reported: (.steps | type == "array")
      }]
    };
  {first: ($first[0] | census | evidence), latest: ($latest[0] | census | evidence),
   source_cause: "unestablished", automatic_source_action: "none"}' > "$scratch/evidence.json"
{
  printf '\nRetry failing jobs:\n%s\n' "$latest"
  # shellcheck disable=SC2016 # Markdown fences are literal, not substitutions.
  printf '\nObserved failure evidence:\n```json\n%s\n```\n' "$(cat "$scratch/evidence.json")"
  printf '\nThe retry remains red. Source causality is unestablished; diagnose the failed steps and submit any source correction through an independently reviewed PR.\n'
} >> "$scratch/body"
gh issue create --repo "$GH_REPO" --title "[CI] Persistent main failure at ${sha:0:12}" --body-file "$scratch/body"
