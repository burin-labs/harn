#!/usr/bin/env bash
# Rehearses a downstream consumer's update against the exact revision under
# test, and reports only the verdict, the consumer run's link and the wall time.
#
# A change that breaks a consumer (a removed flag, a lint promoted to an
# error, a new required field) otherwise surfaces days later, in the
# consumer's own update, many commits wide. This dispatches the consumer's
# update rehearsal at this revision and waits for its terminal state, so the
# breakage belongs to the change that caused it.
#
# The consumer may be private. Nothing it produces is copied here: no log
# line, no step summary, no artifact. The consumer run's link is the only
# pointer, and reading it needs access to the consumer.
#
# A change that needs a matching consumer change names it with a trailer in
# its description or commit messages:
#
#   Pairs-with: <consumer-repo-name>#<pull-request-number>
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
#   TARGET_VERSION     the workspace version at that commit.
#   PAIRING_TEXT       description and commit messages to read trailers from.
#   GH_TOKEN           may dispatch and read the consumer's workflow runs.
#   CANARY_POLL_SECONDS, CANARY_DEADLINE_SECONDS  optional overrides.
set -euo pipefail

canary_fail() {
  echo "::error::CONSUMER_CANARY reason=$1${2:+ $2}"
  exit 1
}

# Sets CANARY_PAIRED to the paired pull request number, or empty when
# unpaired. Two different pairings cannot both be rehearsed in one run, so
# that is a named failure rather than a silent pick.
canary_read_pairing() {
  local consumer_name=$1 text=$2 pairs
  CANARY_PAIRED=
  pairs=$(grep -E '^[[:space:]]*Pairs-with:' <<< "$text" | tr -d '\r' || true)
  [[ -z "$pairs" ]] && return 0
  local numbers
  numbers=$(sed -nE "s/^[[:space:]]*Pairs-with:[[:space:]]*${consumer_name}#([0-9]+)[[:space:]]*$/\1/p" \
    <<< "$pairs" | sort -u)
  if [[ -z "$numbers" ]]; then
    canary_fail pairing_unrecognized "expected=Pairs-with:${consumer_name}#N"
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
  [[ -n "$workflow" ]] || canary_fail consumer_workflow_unset
  [[ "$revision" =~ ^[0-9a-f]{40}$ ]] || canary_fail source_revision_invalid
  [[ -n "$version" ]] || canary_fail target_version_unset

  local paired ref label=default
  canary_read_pairing "${repo#*/}" "${PAIRING_TEXT:-}"
  paired=$CANARY_PAIRED
  if [[ -n "$paired" ]]; then
    local pull state head_repo
    label=pull-$paired
    pull=$(gh api "repos/$repo/pulls/$paired" \
      --jq '"\(.state) \(.head.repo.full_name // "none") \(.head.ref)"') \
      || canary_fail paired_pull_unreadable "pull=$paired"
    read -r state head_repo ref <<< "$pull"
    [[ "$state" == open ]] || canary_fail paired_pull_not_open "pull=$paired state=$state"
    [[ "$head_repo" == "$repo" ]] || canary_fail paired_pull_from_fork "pull=$paired"
  else
    ref=$(gh api "repos/$repo" --jq .default_branch) \
      || canary_fail consumer_unreadable
  fi

  local started dispatched url run_id
  started=$(date +%s)
  dispatched=$(gh workflow run "$workflow" -R "$repo" --ref "$ref" \
    -f target="$version" -f source_revision="$revision" -f legs=clean 2>&1) \
    || canary_fail dispatch_refused "detail=$(head -1 <<< "$dispatched")"
  url=$(grep -oE "https://github\.com/$repo/actions/runs/[0-9]+" <<< "$dispatched" | head -1 || true)
  [[ -n "$url" ]] || canary_fail dispatch_returned_no_run
  run_id=${url##*/}
  echo "CONSUMER_CANARY dispatched run=$url ref=$label"

  local status conclusion now
  while true; do
    sleep "$poll"
    now=$(date +%s)
    if ! read -r status conclusion < <(gh api "repos/$repo/actions/runs/$run_id" \
      --jq '"\(.status) \(.conclusion // "none")"'); then
      status=unreadable
    fi
    [[ "$status" == completed ]] && break
    if ((now - started > deadline)); then
      canary_fail no_verdict_before_deadline "run=$url status=$status wall_seconds=$((now - started))"
    fi
  done

  local verdict=fail
  [[ "$conclusion" == success ]] && verdict=pass
  echo "CONSUMER_CANARY verdict=$verdict conclusion=$conclusion run=$url wall_seconds=$((now - started))"
  [[ "$verdict" == pass ]] || canary_fail consumer_rehearsal_failed "run=$url conclusion=$conclusion"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  canary_main
fi
