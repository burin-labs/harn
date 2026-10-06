#!/usr/bin/env bash
# Rehearses a downstream consumer's update against the exact revision under
# test, and reports only the verdict, the consumer run's id and the wall time.
#
# A change that breaks a consumer (a removed flag, a lint promoted to an
# error, a new required field) otherwise surfaces days later, in the
# consumer's own update, many commits wide. This dispatches the consumer's
# update rehearsal at this revision and waits for its terminal state, so the
# breakage belongs to the change that caused it.
#
# The consumer may be private and this output is public. Nothing it produces
# is copied here: no log line, no step summary, no artifact. Its name never
# appears either: the run is reported by id, and every line goes through
# canary_say, which refuses a line that contains the consumer's name.
#
# A change that needs a matching consumer change names it with a trailer in
# its description or commit messages, without naming the consumer:
#
#   Pairs-with: consumer#<pull-request-number>
#
# The rehearsal then runs on that pull request's branch instead of the
# consumer's default branch.
#
# Every input that is missing, and every state that is not a completed
# success, fails by name. There is no path that reports green without a
# consumer run that concluded success.
#
# `--decide` runs first, in its own job, and decides whether this run
# rehearses at all. A scheduled run whose main commit already has a settled
# verdict (the "Settled verdict" job of an earlier run of this workflow
# succeeded at that commit) is skipped, and the skip is logged by name with
# both commits. A dispatched run always rehearses.
#
# Decide inputs:
#   EVENT_NAME         schedule or workflow_dispatch.
#   SOURCE_REVISION    main's commit for this run.
#   CURRENT_RUN_ID     this run, excluded from the history.
#   GITHUB_REPOSITORY  this repository; GH_TOKEN may read its workflow runs.
#   GITHUB_OUTPUT      receives run=true|false.
#
# Inputs:
#   CANARY_REPOSITORY  owner/name of the consumer.
#   CANARY_WORKFLOW    the consumer's rehearsal workflow file.
#   SOURCE_REVISION    the commit under test.
#   WORKSPACE_VERSION  the workspace version at that commit, as X.Y.Z[-dev].
#   PAIRING_TEXT       description and commit messages to read trailers from.
#   GH_TOKEN           may dispatch and read the consumer's workflow runs.
#   CANARY_POLL_SECONDS, CANARY_DEADLINE_SECONDS  optional overrides.
#   GITHUB_OUTPUT      when set, receives verdict=pass|fail once the consumer
#                      run concluded; an unmeasured run writes no verdict.
set -euo pipefail

# shellcheck source=scripts/lib/release_version.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/release_version.sh"

# The consumer's repository name, which no output line may contain.
CANARY_SECRET_NAME=

canary_say() {
  if [[ -n "$CANARY_SECRET_NAME" && "$*" == *"$CANARY_SECRET_NAME"* ]]; then
    echo "::error::CONSUMER_CANARY reason=identity_in_output"
    exit 1
  fi
  echo "$*"
}

canary_fail() {
  canary_say "::error::CONSUMER_CANARY reason=$1${2:+ $2}"
  exit 1
}

# Sets CANARY_PAIRED to the paired pull request number, or empty when
# unpaired. Two different pairings cannot both be rehearsed in one run, so
# that is a named failure rather than a silent pick.
canary_read_pairing() {
  local text=$1 pairs
  CANARY_PAIRED=
  pairs=$(grep -E '^[[:space:]]*Pairs-with:' <<< "$text" | tr -d '\r' || true)
  [[ -z "$pairs" ]] && return 0
  local numbers
  numbers=$(sed -nE "s/^[[:space:]]*Pairs-with:[[:space:]]*consumer#([0-9]+)[[:space:]]*$/\1/p" \
    <<< "$pairs" | sort -u)
  if [[ -z "$numbers" ]]; then
    canary_fail pairing_unrecognized "expected=Pairs-with:consumer#N"
  fi
  if (($(wc -l <<< "$numbers") > 1)); then
    canary_fail pairing_ambiguous "pulls=$(paste -sd, - <<< "$numbers")"
  fi
  CANARY_PAIRED=$numbers
}

canary_dispatch() {
  local repo=${CANARY_REPOSITORY:-} workflow=${CANARY_WORKFLOW:-}
  local revision=${SOURCE_REVISION:-} workspace_version=${WORKSPACE_VERSION:-}
  # The consumer's rehearsal now runs two candidate legs that capacity
  # admission can place on one reserved runner, where they run one after the
  # other. The six dispatched runs that concluded between 2026-10-01 16:55Z
  # and 2026-10-02 06:09Z took 69 to 90 minutes, so a 55-minute deadline
  # discarded every verdict as unmeasured. 120 minutes leaves headroom over
  # the 90-minute maximum; the job's timeout in consumer-canary.yml stays 10
  # minutes above it so the deadline, not the runner, names the outcome.
  # The job joins the owner and the secret's name, so an unset secret arrives
  # as "owner/" and must not reach the API as a half-formed repository.
  [[ "$repo" =~ ^[^/]+/[^/]+$ ]] \
    || canary_fail consumer_repository_unset "secret=CONSUMER_CANARY_REPOSITORY"
  CANARY_SECRET_NAME=${repo#*/}
  canary_say "CONSUMER_CANARY consumer=configured secret=CONSUMER_CANARY_REPOSITORY"
  [[ -n "$workflow" ]] || canary_fail consumer_workflow_unset
  [[ "$revision" =~ ^[0-9a-f]{40}$ ]] || canary_fail source_revision_invalid
  # The consumer reads the target as a tag name, so resolve the source's
  # published version before choosing the dispatch target.
  local published_version target_version
  published_version="$(release_published_version_for_workspace "$workspace_version")" \
    || canary_fail workspace_version_unpublished "version=$workspace_version"
  target_version="v$published_version"

  local paired ref label=default
  canary_read_pairing "${PAIRING_TEXT:-}"
  paired=$CANARY_PAIRED
  if [[ -n "$paired" ]]; then
    local pull state head_repo
    label=pull-$paired
    pull=$(gh api "repos/$repo/pulls/$paired" \
      --jq '"\(.state) \(.head.repo.full_name // "none") \(.head.ref)"' 2> /dev/null) \
      || canary_fail paired_pull_unreadable "pull=$paired"
    read -r state head_repo ref <<< "$pull"
    [[ "$state" == open ]] || canary_fail paired_pull_not_open "pull=$paired state=$state"
    [[ "$head_repo" == "$repo" ]] || canary_fail paired_pull_from_fork "pull=$paired"
  else
    ref=$(gh api "repos/$repo" --jq .default_branch 2> /dev/null) \
      || canary_fail consumer_unreadable
  fi

  local started dispatched run_id
  started=$(date +%s)
  dispatched=$(gh workflow run "$workflow" -R "$repo" --ref "$ref" \
    -f target="$target_version" -f source_revision="$revision" -f legs=clean 2>&1) \
    || canary_fail dispatch_refused
  run_id=$(grep -oE "https://github\.com/$repo/actions/runs/[0-9]+" <<< "$dispatched" | head -1 || true)
  run_id=${run_id##*/}
  [[ -n "$run_id" ]] || canary_fail dispatch_returned_no_run
  canary_say "CONSUMER_CANARY dispatched run=$run_id ref=$label"

  CANARY_RUN_ID=$run_id
  CANARY_STARTED_AT=$started
}

# One credential owns at most 45 minutes of observation. Workflow windows
# renew the same scoped App authority and carry the original run and clock.
canary_observe() {
  local repo=${CANARY_REPOSITORY:-} run_id=${CANARY_RUN_ID:-} started=${CANARY_STARTED_AT:-}
  local poll=${CANARY_POLL_SECONDS:-60} deadline=${CANARY_DEADLINE_SECONDS:-7200}
  local window=${CANARY_WINDOW_SECONDS:-2700} window_started
  [[ "$repo" =~ ^[^/]+/[^/]+$ ]] || canary_fail consumer_repository_unset
  CANARY_SECRET_NAME=${repo#*/}
  [[ "$run_id" =~ ^[1-9][0-9]*$ && "$started" =~ ^[1-9][0-9]*$ ]] \
    || canary_fail observation_identity_invalid
  [[ "$window" =~ ^[0-9]+$ && "$window" -le 2700 ]] \
    || canary_fail observation_window_invalid "run=$run_id"
  [[ "$poll" =~ ^[0-9]+$ && "$poll" -le 60 && "$deadline" =~ ^-?[0-9]+$ && "$deadline" -le 7200 ]] \
    || canary_fail observation_budget_invalid "run=$run_id"
  window_started=$(date +%s)
  CANARY_PENDING=false

  local status conclusion now record
  while true; do
    now=$(date +%s)
    ((now >= started)) || canary_fail observation_clock_invalid "run=$run_id"
    if ((now - started >= deadline)); then
      canary_fail no_verdict_before_deadline "run=$run_id verdict=unmeasured wall_seconds=$((now - started))"
    fi
    record=$(gh api "repos/$repo/actions/runs/$run_id" \
      --jq '"\(.status) \(.conclusion // "none")"' 2> /dev/null) \
      || canary_fail consumer_read_refused "run=$run_id verdict=unmeasured"
    read -r status conclusion <<< "$record"
    case "$status" in
      completed|queued|in_progress|waiting|pending|requested) ;;
      *) canary_fail consumer_state_unreported "run=$run_id verdict=unmeasured" ;;
    esac
    [[ "$status" == completed ]] && break
    if ((now - window_started >= window)); then
      CANARY_PENDING=true
      canary_say "CONSUMER_CANARY pending run=$run_id status=$status wall_seconds=$((now - started))"
      return 0
    fi
    sleep "$poll"
  done

  local verdict=fail
  [[ "$conclusion" == success ]] && verdict=pass
  if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    echo "verdict=$verdict" >> "$GITHUB_OUTPUT"
  fi
  canary_say "CONSUMER_CANARY verdict=$verdict conclusion=$conclusion run=$run_id wall_seconds=$((now - started))"
  [[ "$verdict" == pass ]] || canary_fail consumer_rehearsal_failed "run=$run_id conclusion=$conclusion"
}

canary_main() {
  canary_dispatch
  canary_observe
  [[ "$CANARY_PENDING" == false ]] || canary_fail observation_window_elapsed "run=$CANARY_RUN_ID verdict=unmeasured"
}

canary_decide() {
  local event=${EVENT_NAME:-} sha=${SOURCE_REVISION:-} repo=${GITHUB_REPOSITORY:-}
  local current=${CURRENT_RUN_ID:-} output=${GITHUB_OUTPUT:-}
  [[ "$sha" =~ ^[0-9a-f]{40}$ ]] || canary_fail source_revision_invalid
  [[ -n "$repo" ]] || canary_fail repository_unset
  [[ -n "$output" ]] || canary_fail output_unset
  if [[ "$event" != schedule ]]; then
    canary_say "CONSUMER_CANARY run reason=explicit_$event main=$sha"
    echo "run=true" >> "$output"
    return 0
  fi
  local history id head settled last='' last_run=''
  history=$(gh api "repos/$repo/actions/workflows/consumer-canary.yml/runs?branch=main&status=completed&per_page=20" \
    --jq '.workflow_runs[] | "\(.id) \(.head_sha)"' 2> /dev/null) \
    || canary_fail settled_history_unreadable
  while read -r id head; do
    [[ -n "$id" && "$id" != "$current" ]] || continue
    settled=$(gh api "repos/$repo/actions/runs/$id/jobs" \
      --jq '[.jobs[] | select(.name == "Settled verdict" and .conclusion == "success")] | length' \
      2> /dev/null) || canary_fail settled_history_unreadable "run=$id"
    if [[ "$settled" =~ ^[1-9] ]]; then
      last=$head
      last_run=$id
      break
    fi
  done <<< "$history"
  if [[ -z "$last" ]]; then
    canary_say "CONSUMER_CANARY run reason=no_settled_verdict main=$sha"
    echo "run=true" >> "$output"
  elif [[ "$last" == "$sha" ]]; then
    canary_say "CONSUMER_CANARY skipped reason=main_unchanged main=$sha last_settled=$last last_settled_run=$last_run"
    echo "run=false" >> "$output"
  else
    canary_say "CONSUMER_CANARY run reason=main_moved main=$sha last_settled=$last last_settled_run=$last_run"
    echo "run=true" >> "$output"
  fi
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  case "${1:-}" in
    --decide) canary_decide ;;
    --dispatch)
      canary_dispatch
      [[ -n "${GITHUB_OUTPUT:-}" ]] || canary_fail observation_output_unset
      printf 'run_id=%s\nstarted_at=%s\n' "$CANARY_RUN_ID" "$CANARY_STARTED_AT" >> "$GITHUB_OUTPUT"
      ;;
    --observe) canary_observe ;;
    "") canary_main ;;
    *) canary_fail invocation_unrecognized ;;
  esac
fi
