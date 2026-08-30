#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/cancel_superseded_merge_groups.sh --repo OWNER/REPO [--expected-sha SHA] [--apply]

Cancel active merge_group workflow runs whose exact head SHA no longer belongs
to a current merge-queue entry. The default is a read-only plan; --apply sends
the bounded Actions cancellation requests.
USAGE
}

die() {
  printf 'cancel_superseded_merge_groups: %s\n' "$*" >&2
  exit 2
}

repo=""
expected_sha=""
apply=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      repo="${2:-}"
      shift 2
      ;;
    --expected-sha)
      expected_sha="${2:-}"
      shift 2
      ;;
    --apply)
      apply=true
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

[[ "$repo" =~ ^[^/[:space:]]+/[^/[:space:]]+$ ]] || die "--repo must be OWNER/REPO"
if [[ -n "$expected_sha" && ! "$expected_sha" =~ ^[0-9a-fA-F]{40}$ ]]; then
  die "--expected-sha must be a full commit SHA"
fi
expected_sha="$(printf '%s' "$expected_sha" | tr '[:upper:]' '[:lower:]')"

tmp_dir="$(mktemp -d)"
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

owner="${repo%%/*}"
name="${repo#*/}"
queue_json="$tmp_dir/queue.json"
queue_shas="$tmp_dir/queue-shas"
runs="$tmp_dir/runs"

# The dollar-prefixed names are GraphQL variables, not shell expansions.
# shellcheck disable=SC2016
queue_query='query($owner:String!,$name:String!){repository(owner:$owner,name:$name){mergeQueue{entries(first:100){pageInfo{hasNextPage} nodes{headCommit{oid}}}}}}'
if ! gh api graphql -F owner="$owner" -F name="$name" -f query="$queue_query" > "$queue_json"; then
  die "could not read the authoritative merge queue; no runs were canceled"
fi

if ! jq -e '
    ((has("errors") | not) or .errors == null or ((.errors | type) == "array" and (.errors | length) == 0))
    and (.data.repository | type == "object")
    and (.data.repository | has("mergeQueue"))
    and (
      .data.repository.mergeQueue == null
      or (
        (.data.repository.mergeQueue | type) == "object"
        and (.data.repository.mergeQueue | has("entries"))
        and (.data.repository.mergeQueue.entries | type) == "object"
        and (.data.repository.mergeQueue.entries | has("pageInfo"))
        and (.data.repository.mergeQueue.entries.pageInfo | type) == "object"
        and (.data.repository.mergeQueue.entries.pageInfo | has("hasNextPage"))
        and .data.repository.mergeQueue.entries.pageInfo.hasNextPage == false
        and (.data.repository.mergeQueue.entries | has("nodes"))
        and (.data.repository.mergeQueue.entries.nodes | type == "array")
        and all(
          .data.repository.mergeQueue.entries.nodes[];
          (type == "object")
          and has("headCommit")
          and (.headCommit == null
            or (
              (.headCommit | type) == "object"
              and (.headCommit | has("oid"))
              and (.headCommit.oid | type == "string")
              and (.headCommit.oid | test("^[0-9a-fA-F]{40}$"))
            ))
        )
      )
    )
  ' "$queue_json" >/dev/null; then
  die "merge-queue response was missing, paginated, or invalid; no runs were canceled"
fi

if jq -e '.data.repository.mergeQueue == null' "$queue_json" >/dev/null; then
  printf 'summary queue_state=disabled active_runs=not_queried action=none reason=nothing_to_supersede apply=%s\n' \
    "$apply"
  exit 0
fi

if ! jq -r '
    (.data.repository.mergeQueue.entries.nodes // [])[]
    | .headCommit.oid? // empty
    | ascii_downcase
  ' "$queue_json" | sort -u > "$queue_shas"; then
  die "merge-queue response contained an invalid head identity; no runs were canceled"
fi

if [[ -n "$expected_sha" ]] && ! grep -Fxq "$expected_sha" "$queue_shas"; then
  die "event head $expected_sha is not authoritative in the current queue; no runs were canceled"
fi

: > "$runs"
for run_status in requested waiting queued pending in_progress; do
  if ! gh api --paginate \
    "repos/$repo/actions/runs?event=merge_group&status=$run_status&per_page=100" \
    --jq '.workflow_runs[] | select(.event == "merge_group") | [.id, (.head_sha | ascii_downcase), .status, .name, (.created_at | fromdateiso8601)] | @tsv' \
    >> "$runs"; then
    die "could not inventory active merge-group runs; no cancellation plan was applied"
  fi
done
sort -u -o "$runs" "$runs"

current_count="$(wc -l < "$queue_shas" | tr -d ' ')"
stale_count=0
cancelled_count=0
failed_count=0
zombie_count=0
now_epoch="$(date +%s)"
queued_zombie_age_seconds=86400

while IFS=$'\t' read -r run_id head_sha run_status workflow_name created_epoch; do
  [[ -n "$run_id" ]] || continue
  [[ "$run_id" =~ ^[0-9]+$ ]] || die "active run inventory contained an invalid run id"
  [[ "$head_sha" =~ ^[0-9a-f]{40}$ ]] || die "active run $run_id had an invalid head SHA"
  [[ "$created_epoch" =~ ^[0-9]+$ ]] || die "active run $run_id had an invalid creation time"
  if grep -Fxq "$head_sha" "$queue_shas"; then
    printf 'preserve run=%s sha=%s status=%s workflow=%s\n' \
      "$run_id" "$head_sha" "$run_status" "$workflow_name"
    continue
  fi

  stale_count=$((stale_count + 1))
  if [[ "$run_status" == "queued" \
    && $((now_epoch - created_epoch)) -gt "$queued_zombie_age_seconds" ]]; then
    jobs_total="$(
      gh api "repos/$repo/actions/runs/$run_id/jobs?per_page=1" \
        --jq '.total_count' 2>/dev/null || true
    )"
    if [[ "$jobs_total" == "0" ]]; then
      # GitHub can retain a jobless queued record for months after destroying
      # its ref while both cancellation endpoints return 500. It consumes no
      # runner and is not an executing proof; classify it separately so one
      # API zombie cannot make the controller fail every five minutes.
      zombie_count=$((zombie_count + 1))
      printf 'zombie run=%s sha=%s status=%s workflow=%s action=none\n' \
        "$run_id" "$head_sha" "$run_status" "$workflow_name"
      continue
    fi
  fi
  printf 'stale run=%s sha=%s status=%s workflow=%s action=%s\n' \
    "$run_id" "$head_sha" "$run_status" "$workflow_name" \
    "$([[ "$apply" == "true" ]] && printf cancel || printf plan)"
  [[ "$apply" == "true" ]] || continue

  if gh api --method POST "repos/$repo/actions/runs/$run_id/cancel" >/dev/null; then
    cancelled_count=$((cancelled_count + 1))
    continue
  fi

  # GitHub can leave a queued run without jobs after its merge-group ref is
  # destroyed. The ordinary endpoint returns 500 for that state; the Actions
  # API exposes force-cancel specifically to retire it without deleting proof.
  if gh api --method POST "repos/$repo/actions/runs/$run_id/force-cancel" >/dev/null; then
    cancelled_count=$((cancelled_count + 1))
    printf 'force-cancelled run=%s\n' "$run_id"
    continue
  fi

  # Completion can win the race after inventory. Only that terminal state is
  # benign; every other failed cancellation remains visible and fails the job.
  latest_status="$(gh api "repos/$repo/actions/runs/$run_id" --jq '.status' 2>/dev/null || true)"
  if [[ "$latest_status" == "completed" ]]; then
    printf 'settled run=%s status=completed\n' "$run_id"
    continue
  fi

  # An OBSERVED zombie, as opposed to the predicted one above. The jobless
  # pre-check catches the common shape, but GitHub also retains long-queued
  # records that DO carry a job row, and those refuse both cancellation
  # endpoints in exactly the same way. Predicting unkillability from a job
  # count was too narrow to hold: one such run failed this controller on every
  # merge group for over a day while cancelling nothing, which is precisely the
  # state in which a genuinely stuck run would go unnoticed.
  #
  # Deciding AFTER both endpoints have actually refused needs no prediction at
  # all, and keeps the alarm meaningful: a run that is recent, or not queued,
  # or that fails cancellation for any other reason still fails the job.
  if [[ "$run_status" == "queued" && "$latest_status" == "queued" \
    && $((now_epoch - created_epoch)) -gt "$queued_zombie_age_seconds" ]]; then
    zombie_count=$((zombie_count + 1))
    printf 'zombie run=%s sha=%s status=%s workflow=%s action=none reason=uncancellable\n' \
      "$run_id" "$head_sha" "$run_status" "$workflow_name"
    continue
  fi

  printf 'failed run=%s latest_status=%s\n' "$run_id" "${latest_status:-unknown}" >&2
  failed_count=$((failed_count + 1))
done < "$runs"

printf 'summary queue_state=configured current_heads=%s active_runs=%s stale_runs=%s cancelled=%s zombies=%s apply=%s\n' \
  "$current_count" "$(wc -l < "$runs" | tr -d ' ')" "$stale_count" \
  "$cancelled_count" "$zombie_count" "$apply"
[[ "$failed_count" -eq 0 ]] || exit 1
