#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/update_queued_pr.sh [options]

Atomically replace the source revision of an open, queued pull request:
  1. dequeue the exact pull request after proving its merge-queue entry;
  2. wait until GitHub reports the entry absent;
  3. replace the proven old head under a force-with-lease and prove the remote OID;
  4. wait for a pull_request workflow run on that exact OID;
  5. if GitHub drops synchronize fanout, close/reopen once and prove recovery;
  6. re-enable squash auto-merge only after the exact-head run exists.

Options:
  --repo OWNER/REPO       Repository (default: current gh repository)
  --pr NUMBER             Pull request (default: PR for current branch)
  --remote NAME           Git remote to push (default: origin)
  --timeout SECONDS       Per-state-transition timeout (default: 60)
  --poll-interval SECONDS Poll interval (default: 2; 0 is useful in tests)
  --no-requeue            Leave the proven exact-head PR out of the queue
  -h, --help              Show this help

This command is intentionally live: invoking it authorizes the dequeue, push,
bounded close/reopen fanout recovery, and auto-merge mutations described above.
USAGE
}

die() {
  printf 'update_queued_pr: %s\n' "$*" >&2
  exit 2
}

GH_BIN="${HARN_QUEUED_PR_GH_BIN:-gh}"
GIT_BIN="${HARN_QUEUED_PR_GIT_BIN:-git}"
repo=""
pr_number=""
remote="origin"
timeout=60
poll_interval=2
requeue=true

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      repo="${2:-}"
      shift 2
      ;;
    --pr)
      pr_number="${2:-}"
      shift 2
      ;;
    --remote)
      remote="${2:-}"
      shift 2
      ;;
    --timeout)
      timeout="${2:-}"
      shift 2
      ;;
    --poll-interval)
      poll_interval="${2:-}"
      shift 2
      ;;
    --no-requeue)
      requeue=false
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: $1"
      ;;
  esac
done

[[ "$timeout" =~ ^[0-9]+$ ]] || die "--timeout must be a non-negative integer"
[[ "$poll_interval" =~ ^[0-9]+([.][0-9]+)?$ ]] || die "--poll-interval must be non-negative"
command -v "$GH_BIN" >/dev/null 2>&1 || die "gh is required"
command -v "$GIT_BIN" >/dev/null 2>&1 || die "git is required"
command -v jq >/dev/null 2>&1 || die "jq is required"

if [[ -z "$repo" ]]; then
  repo="$($GH_BIN repo view --json nameWithOwner --jq .nameWithOwner)"
fi
[[ "$repo" =~ ^[^/]+/[^/]+$ ]] || die "--repo must be OWNER/REPO"
owner="${repo%%/*}"
name="${repo#*/}"

branch="$($GIT_BIN symbolic-ref --quiet --short HEAD)" || die "HEAD must be on a branch"
[[ -n "$branch" ]] || die "could not resolve current branch"

if [[ -z "$pr_number" ]]; then
  pr_number="$($GH_BIN pr view "$branch" --repo "$repo" --json number --jq .number)"
fi
[[ "$pr_number" =~ ^[1-9][0-9]*$ ]] || die "could not resolve a pull request number"

pr_json="$($GH_BIN pr view "$pr_number" --repo "$repo" \
  --json id,number,state,headRefName,headRefOid,isCrossRepository)"
pr_id="$(jq -r '.id // ""' <<<"$pr_json")"
pr_state="$(jq -r '.state // ""' <<<"$pr_json")"
pr_branch="$(jq -r '.headRefName // ""' <<<"$pr_json")"
old_oid="$(jq -r '.headRefOid // ""' <<<"$pr_json")"
is_cross_repo="$(jq -r '.isCrossRepository // false' <<<"$pr_json")"
[[ "$pr_state" == "OPEN" ]] || die "PR #$pr_number is not open"
[[ -n "$pr_id" ]] || die "could not resolve PR #$pr_number node ID"
[[ "$old_oid" =~ ^[0-9a-f]{40}$|^[0-9a-f]{64}$ ]] || die "PR #$pr_number head did not resolve to a full object ID"
[[ "$is_cross_repo" == "false" ]] || die "cross-repository PRs are not supported"
[[ "$pr_branch" == "$branch" ]] || die "current branch '$branch' is not PR #$pr_number head '$pr_branch'"

# GraphQL variable names are literals passed to GitHub, not shell expansions.
# shellcheck disable=SC2016
queue_query='query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      mergeQueueEntry { id state }
      autoMergeRequest { enabledAt }
    }
  }
}'

queue_state_json() {
  "$GH_BIN" api graphql \
    -f query="$queue_query" \
    -f owner="$owner" \
    -f name="$name" \
    -F number="$pr_number"
}

queue_entry_id() {
  queue_state_json | jq -r '.data.repository.pullRequest.mergeQueueEntry.id // ""'
}

deadline_after_timeout() {
  printf '%s\n' "$(( $(date +%s) + timeout ))"
}

wait_until() {
  local description="$1"
  shift
  local deadline
  deadline="$(deadline_after_timeout)"
  while true; do
    if "$@"; then
      return 0
    fi
    if (( $(date +%s) >= deadline )); then
      break
    fi
    sleep "$poll_interval"
  done
  die "timed out waiting for $description"
}

queue_absent() {
  [[ -z "$(queue_entry_id)" ]]
}

queue_or_auto_merge_present() {
  local state
  state="$(queue_state_json)"
  [[ "$(jq -r '[(.data.repository.pullRequest.mergeQueueEntry.id // ""), (.data.repository.pullRequest.autoMergeRequest.enabledAt // "")] | any(length > 0)' <<<"$state")" == "true" ]]
}

initial_entry_id="$(queue_entry_id)"
[[ -n "$initial_entry_id" ]] || die "PR #$pr_number is not currently in the merge queue"

# GitHub names this mutation after the queue operation, but its input is the
# PullRequest node ID rather than the MergeQueueEntry node ID. Keep the entry
# proof above so we still fail closed when the PR is not actually queued.
# shellcheck disable=SC2016
dequeue_mutation='mutation($pullRequestId: ID!) {
  dequeuePullRequest(input: {id: $pullRequestId}) {
    clientMutationId
  }
}'

printf 'dequeue: PR #%s entry %s\n' "$pr_number" "$initial_entry_id"
"$GH_BIN" api graphql \
  -f query="$dequeue_mutation" \
  -f pullRequestId="$pr_id" \
  >/dev/null
wait_until "merge-queue entry removal" queue_absent

new_oid="$($GIT_BIN rev-parse HEAD)"
[[ "$new_oid" =~ ^[0-9a-f]{40}$|^[0-9a-f]{64}$ ]] || die "HEAD did not resolve to a full object ID"

printf 'push: %s HEAD (%s) -> %s\n' "$remote" "$new_oid" "$branch"
# The branch may have been intentionally rebased while the queued snapshot was
# under test. A plain push cannot update that non-fast-forward history; an
# unqualified force could overwrite a concurrent human update. The head OID
# proven before dequeue is the exact lease, so both cases are safe.
"$GIT_BIN" push --no-verify \
  "--force-with-lease=refs/heads/$branch:$old_oid" \
  "$remote" \
  "HEAD:refs/heads/$branch"
remote_oid="$($GIT_BIN ls-remote --heads "$remote" "refs/heads/$branch" | awk 'NR == 1 {print $1}')"
[[ "$remote_oid" == "$new_oid" ]] || die "remote branch proof failed: expected $new_oid, observed ${remote_oid:-absent}"

pr_head_matches() {
  local observed
  observed="$($GH_BIN pr view "$pr_number" --repo "$repo" --json headRefOid --jq .headRefOid)"
  [[ "$observed" == "$new_oid" ]]
}
wait_until "PR head $new_oid" pr_head_matches

exact_head_run_exists() {
  local response
  response="$($GH_BIN api "/repos/$repo/actions/runs" \
    --method GET \
    -f head_sha="$new_oid" \
    -f event=pull_request \
    -f per_page=1)"
  [[ "$(jq -r --arg oid "$new_oid" '[.workflow_runs[]? | select(.head_sha == $oid and .event == "pull_request")] | length' <<<"$response")" -gt 0 ]]
}

fanout_recovered=false
fanout_deadline="$(deadline_after_timeout)"
while true; do
  if exact_head_run_exists; then
    fanout_recovered=true
    break
  fi
  if (( $(date +%s) >= fanout_deadline )); then
    break
  fi
  sleep "$poll_interval"
done

if [[ "$fanout_recovered" != "true" ]]; then
  printf 'fanout: no exact-head pull_request run; closing and reopening PR #%s once\n' "$pr_number"
  "$GH_BIN" pr close "$pr_number" --repo "$repo" >/dev/null
  "$GH_BIN" pr reopen "$pr_number" --repo "$repo" >/dev/null
  wait_until "pull_request workflow fanout for $new_oid after reopen" exact_head_run_exists
  fanout_recovered=true
fi

if [[ "$requeue" == "true" ]]; then
  printf 'requeue: enabling squash auto-merge for exact head %s\n' "$new_oid"
  "$GH_BIN" pr merge "$pr_number" --repo "$repo" --auto --squash
  wait_until "merge queue or auto-merge state" queue_or_auto_merge_present
fi

printf 'result=success\n'
printf 'pr_number=%s\n' "$pr_number"
printf 'branch=%s\n' "$branch"
printf 'previous_head_oid=%s\n' "$old_oid"
printf 'head_oid=%s\n' "$new_oid"
printf 'remote_oid=%s\n' "$remote_oid"
printf 'exact_head_pull_request_run=true\n'
printf 'requeued=%s\n' "$requeue"
