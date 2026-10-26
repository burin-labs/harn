#!/usr/bin/env bash
# A release needs a completed main-push CI run at its immutable source.
set -euo pipefail
main_only=false
case "$#:$*" in
  0:) ;;
  1:--main-only) main_only=true ;;
  *) echo 'usage: require_full_suite.sh [--main-only]' >&2; exit 2 ;;
esac
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=scripts/ci/require_native_qualification.sh
source "$script_dir/require_native_qualification.sh"
if [[ -f "$script_dir/../lib/full_suite_jobs.sh" ]]; then
  # shellcheck source=scripts/lib/full_suite_jobs.sh
  source "$script_dir/../lib/full_suite_jobs.sh"
else
  # shellcheck source=scripts/lib/full_suite_jobs.sh
  source "$script_dir/lib/full_suite_jobs.sh"
fi
policy_file="$script_dir/release_audit_contract.json"
if [[ ! -f "$policy_file" ]]; then
  policy_file="$script_dir/../release_audit_contract.json"
fi
: "${GH_REPO:?repository required}"
: "${SOURCE_SHA:?source required}"
[[ "$SOURCE_SHA" =~ ^[0-9a-f]{40}$ ]] || exit 1
required_jobs=$(printf '%s\n' "${RELEASE_FULL_SUITE_JOBS[@]}" | jq -Rsc 'split("\n")[:-1]')
report() {
  if [[ -n "${FULL_SUITE_STATUS_FILE:-}" ]]; then
    printf '%s\n' "$1" > "$FULL_SUITE_STATUS_FILE"
  fi
  printf '%s\n' "$1"
}
unmeasured() {
  report "$(jq -nc --arg source "$SOURCE_SHA" --arg reason "$1" \
    '{kind:"harn.release.full_suite.v1",stage:"source-ci",source_sha:$source,verdict:"unmeasured",reason:$reason,
      run_id:null,pending_count:null,failed_count:null,pending_jobs:[],failing_jobs:[],missing_jobs:[],next_action:"retry-read"}')"
}
if [[ -n "${FULL_SUITE_STATUS_FILE:-}" ]]; then
  # A killed or timed-out read must leave an explicit unmeasured receipt.
  unmeasured ci-read-started >/dev/null
fi
if [[ "${WAIT_FOR_FULL_SUITE:-false}" == true ]]; then
  if ! active=$(gh api "repos/$GH_REPO/actions/workflows/ci.yml/runs?event=push&branch=main&head_sha=$SOURCE_SHA&per_page=100" --jq '.workflow_runs | sort_by(.id) | last | select(.status != "completed") | .id // empty'); then
    unmeasured runs
    exit 1
  fi
  if [[ -n "$active" ]]; then
    # The terminal records decide proof, including a failed watched run.
    if ! release_watch_run "$GH_REPO" "$active"; then
      unmeasured watch
      exit 1
    fi
  fi
fi
if ! runs=$(gh api --paginate "repos/$GH_REPO/actions/workflows/ci.yml/runs?event=push&branch=main&head_sha=$SOURCE_SHA&per_page=100" --jq '.workflow_runs[] | [.id, .head_sha, .status, .conclusion] | @tsv'); then
  unmeasured runs
  exit 1
fi
observed=0
while IFS=$'\t' read -r id sha status conclusion; do
  [[ "$sha" == "$SOURCE_SHA" ]] || continue
  [[ "$id" =~ ^[0-9]+$ ]] || { unmeasured run-identity; exit 1; }
  observed=$((observed + 1))
  if ! jobs=$(gh api --paginate "repos/$GH_REPO/actions/runs/$id/jobs?filter=latest&per_page=100" --jq '.jobs[] | [.name, .status, .conclusion] | @tsv'); then
    unmeasured jobs
    exit 1
  fi
  verdict=$(jq -Rsc --arg source "$SOURCE_SHA" --argjson run "$id" --arg status "$status" \
    --arg conclusion "$conclusion" --argjson required "$required_jobs" '
    split("\n") | map(select(length > 0) | split("\t") | {name:.[0],status:.[1],conclusion:.[2]}) as $jobs |
    [$required[] | . as $name | [$jobs[] | select(.name == $name)] as $matches |
      if ($matches|length) == 1 then $matches[0] else {name:$name,status:"missing-or-duplicate",conclusion:null} end] as $conditions |
    {kind:"harn.release.full_suite.v1",stage:"source-ci",source_sha:$source,run_id:$run,
      pending_jobs:[$conditions[]|select(.status != "completed")|.name],
      failing_jobs:[$conditions[]|select(.status == "completed" and .conclusion != "success")|.name],
      missing_jobs:[$conditions[]|select(.status == "missing-or-duplicate")|.name]} |
    .pending_count = (.pending_jobs|length) | .failed_count = (.failing_jobs|length) |
    .verdict = (if $status == "completed" and $conclusion == "success" and .pending_count == 0 and .failed_count == 0 then "success"
      elif $status != "completed" then "pending" else "failure" end) |
    .next_action = (if .verdict == "success" then "continue" elif .verdict == "pending" then "wait-for-ci" else "repair-source" end)
  ' <<< "$jobs")
  report "$verdict"
  if jq -e '.verdict == "success"' <<< "$verdict" >/dev/null; then
    if [[ "$main_only" != true ]]; then
      require_native_qualification "$GH_REPO" "$SOURCE_SHA" "$policy_file"
    fi
    echo "Full-suite CI succeeded at $SOURCE_SHA: https://github.com/$GH_REPO/actions/runs/$id"
    exit 0
  fi
done <<< "$runs"
if (( observed == 0 )); then
  report "$(jq -nc --arg source "$SOURCE_SHA" --argjson required "$required_jobs" \
    '{kind:"harn.release.full_suite.v1",stage:"source-ci",source_sha:$source,verdict:"failure",run_id:null,
      pending_count:($required|length),failed_count:0,pending_jobs:$required,failing_jobs:[],missing_jobs:$required,next_action:"run-main-ci"}')"
fi
echo "No green full-suite main push at $SOURCE_SHA; release refused." >&2
exit 1
