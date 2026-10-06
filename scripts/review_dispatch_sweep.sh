#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/review_dispatch_sweep.sh --repo OWNER/REPO [--now ISO-8601] [--limit N] [--apply]

Find ready pull requests whose current head never got an automatic review
outcome, and request one. The pull request events in review-dispatch.yml
request a review once per head; this sweep covers the two ways that request
ends with nothing:

  daily_cap   the reviewer stood the head down at its daily limit. The head is
              requested again once per UTC day after the limit resets.
  no_outcome  the head has no review and no stand-down, and its latest request
              is at least two hours old. The reviewer's admission queue keeps
              one pending run, so a burst of requests cancels the older ones
              without a word on the pull request.

A head counts as decided when the reviewer App left a review on it or posted
any other stand-down for it (draft, opt-out label, generated-only, ...). Every
request, from a pull request event or from this sweep, is recorded as an
`Automatic review: requested` check run on the head; the sweep reads those to
space its requests.

The default is a read-only plan: one line per candidate, oldest pull request
first. --apply dispatches up to --limit (default 4) of them, waiting
REVIEW_SWEEP_SPACING_SECONDS (default 90) between requests so the reviewer's
admission queue never holds two pending runs. --apply reads
REVIEW_REPOSITORY, REVIEW_WORKFLOW, optional REVIEW_REF, and DISPATCH_TOKEN
(the App token that may dispatch the review workflow); GH_TOKEN reads this
repository and records the check run.
USAGE
}

die() {
  printf 'review_dispatch_sweep: %s\n' "$*" >&2
  exit 2
}

repo=""
now=""
limit=4
apply=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      repo="${2:-}"
      shift 2
      ;;
    --now)
      now="${2:-}"
      shift 2
      ;;
    --limit)
      limit="${2:-}"
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
[[ "$limit" =~ ^[1-9][0-9]?$ ]] || die "--limit must be between 1 and 99"
if [[ -z "$now" ]]; then
  now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
fi
[[ "$now" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$ ]] \
  || die "--now must be a UTC timestamp like 2026-10-06T03:00:00Z"
spacing="${REVIEW_SWEEP_SPACING_SECONDS:-90}"
[[ "$spacing" =~ ^[0-9]+$ ]] || die "REVIEW_SWEEP_SPACING_SECONDS must be a whole number"

request_check="Automatic review: requested"
owner="${repo%%/*}"
name="${repo#*/}"

# The census. Oldest first, so a backlog drains in the order it formed.
# shellcheck disable=SC2016 # GraphQL variables, not shell expansions.
query='query($owner: String!, $name: String!) {
  repository(owner: $owner, name: $name) {
    pullRequests(states: OPEN, first: 50, orderBy: {field: CREATED_AT, direction: ASC}) {
      pageInfo { hasNextPage }
      nodes {
        number
        isDraft
        isCrossRepository
        headRefOid
        author { __typename }
        reviews(last: 15) { nodes { author { __typename } commit { oid } } }
        comments(last: 20) { nodes { author { __typename } createdAt body } }
      }
    }
  }
}'

response="$(gh api graphql -f query="$query" -f owner="$owner" -f name="$name")" \
  || die "could not read the open pull requests of $repo"

# Heads still waiting: ready, same-repository, person-authored, and without a
# review from a bot on that exact head. One JSON object per head, carrying the
# kind of the reviewer's latest stand-down for it, if any.
waiting="$(jq -c '
  if .data.repository.pullRequests == null then error("no pull requests in the response") else . end
  | .data.repository.pullRequests.nodes[]
  | select(.isDraft | not)
  | select(.isCrossRepository | not)
  | select(.author.__typename != "Bot")
  | .headRefOid as $head
  | select([.reviews.nodes[] | select(.author.__typename == "Bot" and .commit.oid == $head)] | length == 0)
  | ([.comments.nodes[]
      | select(.author.__typename == "Bot")
      | select(.body | contains("<!-- automated-review-stand-down: " + $head + " -->"))]
      | sort_by(.createdAt) | last) as $stand_down
  | {number, head: $head,
     stand_down: (if $stand_down == null then null
                  else ($stand_down.body | capture("\\(`(?<kind>[a-z_]+)`\\)").kind // "unknown") end)}
' <<<"$response")" || die "could not read the pull request census"

# Each waiting head's latest request, from its check runs.
census=""
while IFS= read -r entry; do
  [[ -n "$entry" ]] || continue
  head="$(jq -r '.head' <<<"$entry")"
  requests="$(gh api "repos/$repo/commits/$head/check-runs?check_name=$(jq -rn --arg c "$request_check" '$c | @uri')&per_page=100" \
    --jq '[.check_runs[].started_at | select(. != null)] | max')" \
    || die "could not read the review requests of $head"
  census+="$(jq -c --arg last "$requests" '. + {last_request: (if $last == "" or $last == "null" then null else $last end)}' <<<"$entry")"$'\n'
done <<<"$waiting"

# The decision. Prints `number<TAB>head<TAB>reason` per candidate.
plan="$(jq -rs --arg now "$now" '
  def epoch: sub("\\.[0-9]+Z$"; "Z") | fromdateiso8601;
  ($now | epoch) as $now_s
  | ($now[0:10] + "T00:00:00Z" | epoch) as $day_start
  | .[]
  | (if .last_request == null then null else (.last_request | epoch) end) as $last
  | (if .stand_down == null or .stand_down == "review_unmeasured" then
       (if $last == null or $now_s - $last >= 7200 then (.stand_down // "no_outcome") else empty end)
     elif .stand_down == "daily_cap" then
       (if $last == null or $last < $day_start then "daily_cap" else empty end)
     else empty end) as $reason
  | [.number, .head, $reason] | @tsv
' <<<"$census")" || die "could not decide the review requests"

if [[ "$(jq -r '.data.repository.pullRequests.pageInfo.hasNextPage' <<<"$response")" == "true" ]]; then
  printf 'review_dispatch_sweep: more than 50 open pull requests; only the oldest 50 were considered\n' >&2
fi

if [[ -z "$plan" ]]; then
  printf 'No pull request head is waiting for an automatic review outcome.\n'
  exit 0
fi

printf '%s\n' "$plan"
[[ "$apply" == true ]] || exit 0

: "${REVIEW_REPOSITORY:?REVIEW_REPOSITORY is required with --apply}"
: "${REVIEW_WORKFLOW:?REVIEW_WORKFLOW is required with --apply}"
: "${DISPATCH_TOKEN:?DISPATCH_TOKEN is required with --apply}"
ref_args=()
if [[ -n "${REVIEW_REF:-}" ]]; then
  ref_args=(--ref "$REVIEW_REF")
fi

sent=0
while IFS=$'\t' read -r number head reason; do
  [[ "$sent" -lt "$limit" ]] || break
  if [[ "$sent" -gt 0 && "$spacing" -gt 0 ]]; then
    sleep "$spacing"
  fi
  GH_TOKEN="$DISPATCH_TOKEN" gh workflow run "$REVIEW_WORKFLOW" \
    --repo "$REVIEW_REPOSITORY" \
    "${ref_args[@]}" \
    -f repository="$repo" \
    -f pr="$number" \
    -f head_sha="$head"
  gh api "repos/$repo/check-runs" \
    -f name="$request_check" \
    -f head_sha="$head" \
    -f status=completed \
    -f conclusion=success \
    -f "output[title]=Requested again by the review sweep" \
    -f "output[summary]=The earlier request for this head ended without a review ($reason)." \
    >/dev/null
  printf 'Requested a review of %s#%s at %s (%s).\n' "$repo" "$number" "$head" "$reason"
  sent=$((sent + 1))
done <<<"$plan"
