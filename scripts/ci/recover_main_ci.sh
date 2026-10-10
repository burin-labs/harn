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
# A repeated job name can be a repeated setup/API failure. Require the same
# registered source assertion to have actually failed on both attempts.
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
  {first: ($first[0] | census), latest: ($latest[0] | census)}' \
  | jq --slurpfile policy scripts/ci/main_recovery_policy.json -f scripts/ci/main_recovery_evidence.jq > "$scratch/evidence.json"
printf '\nRetry failing jobs:\n%s\n' "$latest" >> "$scratch/body"
# shellcheck disable=SC2016 # Markdown fences are literal, not substitutions.
printf '\nSource assertion evidence:\n```json\n%s\n```\n' "$(cat "$scratch/evidence.json")" >> "$scratch/body"
if [[ "$(jq -r '.authorized' "$scratch/evidence.json")" != true ]]; then
  printf '\nNo repeated source assertion was established; setup, measurement and unregistered checks need investigation, not a source revert.\n' >> "$scratch/body"
  gh issue create --repo "$GH_REPO" --title "[CI] Unstable main suite at ${sha:0:12}" --body-file "$scratch/body"
  exit 0
fi

# Only a commit whose parent has full-suite proof is a measured culprit.
# A red parent is an existing incident, not evidence against this change.
git cat-file -e "$sha^{commit}"
parent=$(git rev-parse "$sha^1")
branch="automation/revert-main-$sha"
prior=$(gh pr list --repo "$GH_REPO" --head "$branch" --state all --json number --jq '.[0].number // empty')
if [[ -n "$prior" ]]; then
  echo "Culprit already has revert PR #$prior."
  exit 0
fi
if ! SOURCE_SHA="$parent" bash scripts/ci/require_full_suite.sh; then
  printf '\nParent has no green full-suite proof; culprit attribution needs investigation.\n' >> "$scratch/body"
  gh issue create --repo "$GH_REPO" --title "[CI] Persistent main failure at ${sha:0:12}" --body-file "$scratch/body"
  exit 0
fi
base=$(git rev-parse HEAD)
parents=$(git rev-list --parents -n 1 "$sha")
read -ra parent_ids <<< "$parents"
args=()
if (( ${#parent_ids[@]} > 2 )); then args=(-m 1); fi
if ! git revert --no-commit "${args[@]}" "$sha"; then
  git revert --abort
  printf '\nThe culprit revert conflicts with current main and needs recovery.\n' >> "$scratch/body"
  gh issue create --repo "$GH_REPO" --title "[CI] Main culprit revert conflicts at ${sha:0:12}" --body-file "$scratch/body"
  exit 0
fi
HARN_BRANCH_COMMIT_TOKEN="$GH_TOKEN" \
  HARN_BRANCH_COMMIT_BRANCH="$branch" \
  HARN_BRANCH_COMMIT_BASE_OID="$base" \
  HARN_BRANCH_COMMIT_HEADLINE="[CI] Revert failing main commit ${sha:0:12}" \
  "${HARN_BIN:?Harn required}" run --no-sandbox scripts/bump-driver/publish_branch_commit.harn
pr=$(gh pr list --repo "$GH_REPO" --head "$branch" --state all --json number --jq '.[0].number // empty')
if [[ -z "$pr" ]]; then
  pr=$(gh pr create --repo "$GH_REPO" --base main --head "$branch" --title "[CI] Revert failing main commit ${sha:0:12}" --body-file "$scratch/body")
fi
gh pr edit "$pr" --repo "$GH_REPO" --add-label ship
