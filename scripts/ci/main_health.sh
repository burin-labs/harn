#!/usr/bin/env bash
#
# Compute the `main health` verdict and post it as a commit status.
#
# .github/workflows/main-health.yml owns when this runs; this script owns what
# the verdict means. Every watched suite ends in exactly one of four states:
#
#   broken      its newest `health_failures` judged runs all failed
#   unreadable  a request for its history failed, or no such workflow exists
#   unjudged    its history was read but holds no completed outcome, e.g. it
#               has never run on a schedule, or its judged job was skipped
#   ok          judged, and not broken
#
# Only `ok` counts toward success. Any broken or unreadable suite posts
# failure; otherwise any unjudged suite posts pending. Measuring nothing never
# reads as healthy, and a run that dies before deciding still posts failure,
# so a green status carried forward from an older commit cannot outlive it.
#
# Environment: GH_REPO, EVENT_NAME, PUSHED_SHA (for push), HARN (a Harn
# executable), GITHUB_STEP_SUMMARY, GITHUB_RUN_ID. Requires `gh` authenticated
# with statuses: write and actions: read.
set -euo pipefail

today="$(date -u +%F)"
run_url="https://github.com/$GH_REPO/actions/runs/${GITHUB_RUN_ID}"
sha=""
posted=0
# Runs read per suite. The registry caps health_failures at 10 so a run-level
# streak always fits.
RUN_WINDOW=12

post() { # state description
  gh api "repos/$GH_REPO/statuses/$sha" -X POST \
    -f state="$1" -f context="main health" -f description="${2:0:139}" \
    -f target_url="$run_url" >/dev/null
  posted=1
}

# Whatever stops this script before it posts, the commit must not keep a
# verdict nobody measured.
on_exit() {
  local code=$?
  if (( posted == 0 )) && [[ -n "$sha" ]]; then
    post failure "$today: main health could not compute its verdict" || true
    echo "::error::main health could not compute its verdict (exit $code)"
    exit 1
  fi
  exit "$code"
}
trap on_exit EXIT

summary() { echo "$*" >> "$GITHUB_STEP_SUMMARY"; }

if [[ "$EVENT_NAME" == "push" ]]; then
  sha="$PUSHED_SHA"
else
  sha="$(gh api "repos/$GH_REPO/commits/main" --jq '.sha')"
fi

# One suite per line: name, consecutive failures that count as broken, and the
# job judged instead of the whole run (may be empty). The checker validates the
# registry before printing it; its failure ends the run here, as a failure.
suites="$("$HARN" run scripts/check_scheduled_workflows.harn -- --health-suites)"
watched="$(grep -c . <<< "$suites" || true)"
if (( watched == 0 )); then
  echo "::error::scripts/scheduled_workflows.toml watches no suites"
  exit 1
fi

# `gh run list --workflow` returns nothing both for a workflow that does not
# exist and for one that has never run on a schedule, so resolve the names once
# and report a miss rather than letting a rename silently drop a suite.
known="$(gh api "repos/$GH_REPO/actions/workflows" --paginate --jq '.workflows[].name')"

broken=()
unreadable=()
unjudged=()
ok=0

summary "| suite | recent scheduled runs (newest first) | verdict |"
summary "| --- | --- | --- |"

while IFS=$'\t' read -r suite threshold job; do
  [[ -n "$suite" ]] || continue

  # Fixed-string, whole-line match: suite names contain spaces and hyphens, so
  # a substring or regex match would happily accept a renamed neighbour.
  if ! grep -Fxq "$suite" <<< "$known"; then
    unreadable+=("$suite")
    summary "| $suite | — | **UNREADABLE — no such workflow** |"
    continue
  fi

  # Every row is kept here so the window's size is known; in-flight runs
  # (empty conclusion, printed as `-`) and cancelled ones are passed over
  # below so they cannot break a streak that is genuinely unbroken.
  if ! runs="$(
    gh run list --repo "$GH_REPO" --workflow "$suite" \
      --event schedule --branch main --limit "$RUN_WINDOW" \
      --json databaseId,conclusion \
      --jq '.[] | "\(.databaseId) \(if (.conclusion // "") == "" then "-" else .conclusion end)"'
  )"; then
    unreadable+=("$suite")
    summary "| $suite | — | **UNREADABLE — run history request failed** |"
    continue
  fi

  outcomes=()
  job_read_failed=0
  while read -r run_id conclusion; do
    [[ -n "$run_id" ]] || continue
    [[ "$conclusion" != "-" && "$conclusion" != cancelled ]] || continue
    (( ${#outcomes[@]} < threshold )) || break
    if [[ -n "$job" ]]; then
      # A run whose judged job did not run measured nothing and is passed
      # over. A failed request for the job list is not that: it is unreadable.
      if ! conclusion="$(JOB="$job" gh api "repos/$GH_REPO/actions/runs/$run_id/jobs" \
        --jq '[.jobs[] | select(.name == env.JOB and .conclusion != "skipped")][0].conclusion // empty')"; then
        job_read_failed=1
        break
      fi
      [[ -n "$conclusion" ]] || continue
    fi
    outcomes+=("$conclusion")
  done <<< "$runs"

  if (( job_read_failed )); then
    unreadable+=("$suite")
    summary "| $suite | — | **UNREADABLE — job request failed** |"
    continue
  fi

  if (( ${#outcomes[@]} == 0 )); then
    unjudged+=("$suite")
    summary "| $suite | (no completed outcome) | **UNJUDGED** |"
    continue
  fi

  streak=0
  for outcome in "${outcomes[@]}"; do
    [[ "$outcome" == "failure" ]] || break
    streak=$((streak + 1))
  done

  # Every measured outcome read is red, yet there are fewer than the threshold
  # and the window of runs was full, so older runs were never read. That
  # happens when most runs did not measure the judged job; the streak could
  # already be past the threshold, so the suite is unreadable, not ok.
  window_rows="$(grep -c . <<< "$runs" || true)"
  if (( streak < threshold && streak == ${#outcomes[@]} && window_rows >= RUN_WINDOW )); then
    unreadable+=("$suite")
    summary "| $suite | ${outcomes[*]} | **UNREADABLE — only ${#outcomes[@]} measured run(s) in the newest $RUN_WINDOW, all red** |"
    continue
  fi

  if (( streak >= threshold )); then
    broken+=("$suite (${streak} consecutive)")
    summary "| $suite | ${outcomes[*]} | **BROKEN — ${streak} consecutive** |"
  else
    ok=$((ok + 1))
    summary "| $suite | ${outcomes[*]} | ok |"
  fi
done <<< "$suites"

judged=$((ok + ${#broken[@]}))
counts="$judged/$watched judged, ${#unreadable[@]} unreadable, ${#unjudged[@]} unjudged"
summary ""
summary "watched=$watched judged=$judged ok=$ok broken=${#broken[@]} unreadable=${#unreadable[@]} unjudged=${#unjudged[@]}"

join() { local IFS=';'; echo "$*"; }

if (( ${#broken[@]} > 0 || ${#unreadable[@]} > 0 )); then
  problems=("${broken[@]}" "${unreadable[@]/#/unreadable: }" "${unjudged[@]/#/unjudged: }")
  post failure "$today: ${#broken[@]} broken, $counts; $(join "${problems[@]}")"
  echo "::error::main health — $counts — $(join "${problems[@]}")"
  # A push only republishes the verdict; the failing status is the report, and
  # a red push run would double it on every merge.
  [[ "$EVENT_NAME" == "push" ]] || exit 1
  exit 0
fi

if (( ${#unjudged[@]} > 0 )); then
  post pending "$today: $counts; unjudged: $(join "${unjudged[@]}")"
  echo "::warning::main health — $counts"
  exit 0
fi

post success "$today: all $watched watched scheduled suites judged and passing"
summary "All $watched watched suites are passing."
