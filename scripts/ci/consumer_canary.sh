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
# Inputs:
#   CANARY_REPOSITORY  owner/name of the consumer.
#   CANARY_WORKFLOW    the consumer's rehearsal workflow file.
#   SOURCE_REVISION    the commit under test.
#   TARGET_VERSION     the workspace version at that commit, as vX.Y.Z[-pre].
#   PAIRING_TEXT       description and commit messages to read trailers from.
#   GH_TOKEN           may dispatch and read the consumer's workflow runs.
#   CANARY_POLL_SECONDS, CANARY_DEADLINE_SECONDS  optional overrides.
set -euo pipefail

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

canary_main() {
  local repo=${CANARY_REPOSITORY:-} workflow=${CANARY_WORKFLOW:-}
  local revision=${SOURCE_REVISION:-} version=${TARGET_VERSION:-}
  local poll=${CANARY_POLL_SECONDS:-60} deadline=${CANARY_DEADLINE_SECONDS:-2400}
  [[ -n "$repo" ]] || canary_fail consumer_repository_unset
  CANARY_SECRET_NAME=${repo#*/}
  [[ -n "$workflow" ]] || canary_fail consumer_workflow_unset
  [[ "$revision" =~ ^[0-9a-f]{40}$ ]] || canary_fail source_revision_invalid
  # The consumer reads the target as a tag name, so a bare workspace version
  # is refused there; refuse it here first, by name.
  [[ "$version" =~ ^v[0-9]+\.[0-9]+\.[0-9]+ ]] || canary_fail target_version_invalid "target=$version"

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
    -f target="$version" -f source_revision="$revision" -f legs=clean 2>&1) \
    || canary_fail dispatch_refused
  run_id=$(grep -oE "https://github\.com/$repo/actions/runs/[0-9]+" <<< "$dispatched" | head -1 || true)
  run_id=${run_id##*/}
  [[ -n "$run_id" ]] || canary_fail dispatch_returned_no_run
  canary_say "CONSUMER_CANARY dispatched run=$run_id ref=$label"

  local status conclusion now
  while true; do
    sleep "$poll"
    now=$(date +%s)
    if ! read -r status conclusion < <(gh api "repos/$repo/actions/runs/$run_id" \
      --jq '"\(.status) \(.conclusion // "none")"' 2> /dev/null); then
      status=unreadable
    fi
    [[ "$status" == completed ]] && break
    if ((now - started > deadline)); then
      canary_fail no_verdict_before_deadline "run=$run_id status=$status wall_seconds=$((now - started))"
    fi
  done

  local verdict=fail
  [[ "$conclusion" == success ]] && verdict=pass
  canary_say "CONSUMER_CANARY verdict=$verdict conclusion=$conclusion run=$run_id wall_seconds=$((now - started))"
  [[ "$verdict" == pass ]] || canary_fail consumer_rehearsal_failed "run=$run_id conclusion=$conclusion"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  canary_main
fi
