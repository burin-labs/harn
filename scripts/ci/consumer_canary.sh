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
# `--decide` runs first, in its own job, and decides what this run rehearses
# and whether it rehearses at all. Without an explicit revision it rehearses
# `green`, the newest main commit whose full CI suite passed. A scheduled run
# whose revision already has a settled verdict (the "Settled verdict" job of an
# earlier run of this workflow succeeded and its step names that revision) is
# skipped, and the skip is logged by name with both commits. A dispatched run
# always rehearses.
#
# Decide inputs:
#   EVENT_NAME         schedule or workflow_dispatch.
#   SOURCE_REVISION    the caller's exact revision; empty rehearses green.
#   CURRENT_RUN_ID     this run, excluded from the history.
#   GITHUB_REPOSITORY  this repository; GH_TOKEN may read its runs and refs.
#   GITHUB_OUTPUT      receives run=true|false and source_revision.
#
# Inputs:
#   CANARY_REPOSITORY  owner/name of the consumer.
#   CANARY_WORKFLOW    the consumer's rehearsal workflow file.
#   SOURCE_REVISION    the commit under test.
#   WORKSPACE_VERSION  the workspace version at that commit, as X.Y.Z[-dev].
#   PAIRING_TEXT       description and commit messages to read trailers from.
#   REHEARSAL_PROMOTION_RUN_ID  optional prior publication attempt to authenticate.
#   CANDIDATE_RUN_ID   certified producer bound to that prior attempt.
#   REHEARSAL_PROMOTION_READ_TOKEN  scoped read authority for the prior attempt.
#   CANARY_PULL_REQUEST_CLI_RUN, CANARY_PULL_REQUEST_BASE  optional: the CI run
#                      that built a pull request's CLI, and that pull request's base.
#   CANARY_PULL_REQUEST_CLI_ARTIFACT  optional: harn-base-cli.tar.zst when that
#                      run's base CLI is rehearsed at SOURCE_REVISION == the base.
#   CANARY_RELEASE_PRODUCER_RUN_ID, CANARY_RELEASE_PRODUCER_RUN_ATTEMPT
#                      optional current producer whose archives the consumer verifies.
#   GH_TOKEN           may dispatch and read the consumer's workflow runs.
#   CANARY_POLL_SECONDS, CANARY_DEADLINE_SECONDS  optional overrides.
#   GITHUB_OUTPUT      when set, receives verdict=pass|fail once the consumer
#                      run concluded; an unmeasured run writes no verdict.
set -euo pipefail

# shellcheck source=scripts/lib/release_version.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/release_version.sh"
# shellcheck source=scripts/lib/consumer_canary_policy.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/consumer_canary_policy.sh"
# shellcheck source=scripts/lib/green_checkpoint.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/green_checkpoint.sh"

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
  if [[ -n "${CANARY_TIMING_RECEIPT:-}" ]]; then
    canary_say "CONSUMER_CANARY timing=$CANARY_TIMING_RECEIPT"
  fi
  if [[ -n "${CANARY_RUN_ID:-}" ]]; then
    CANARY_RUN_ID="$CANARY_RUN_ID" node "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/consumer-observer-failure.mjs" "$1" || true
  fi
  canary_say "::error::CONSUMER_CANARY reason=$1${2:+ $2}"
  # A refused dispatch has no failure receipt, so it names its reason here.
  if [[ "${CANARY_DISPATCHING:-}" == true && -n "${GITHUB_OUTPUT:-}" ]]; then
    echo "reason=$1" >> "$GITHUB_OUTPUT"
  fi
  exit 1
}

# Records what CI status needs to compare two rehearsals without reading
# consumer content: the run's head commit, a digest of its job names, and the
# named reason an unmeasured attempt ended with. Nothing here is printed.
canary_record() {
  local repo=${CANARY_REPOSITORY:-} run_id=${CANARY_RECORD_RUN:-} receipt=${CANARY_FAILURE_RECEIPT:-}
  local reason=${CANARY_DISPATCH_REASON:-} head='' jobs digest=''
  [[ -n "${GITHUB_OUTPUT:-}" ]] || { echo '::error::CONSUMER_CANARY reason=record_output_unset'; exit 1; }
  if [[ -z "$reason" && -n "$receipt" && -f "$receipt" ]]; then
    reason=$(jq -r '.reason // empty' "$receipt" 2> /dev/null || true)
  fi
  [[ "$reason" =~ ^[a-z_]*$ ]] || reason=failure_receipt_unreadable
  if [[ "$repo" =~ ^[^/]+/[^/]+$ && "$run_id" =~ ^[1-9][0-9]*$ ]]; then
    head=$(gh api "repos/$repo/actions/runs/$run_id" --jq .head_sha 2> /dev/null || true)
    [[ "$head" =~ ^[0-9a-f]{40}$ ]] || head=''
    if jobs=$(gh api --paginate "repos/$repo/actions/runs/$run_id/jobs?filter=latest&per_page=100" \
        --jq '.jobs[] | select(.conclusion != "skipped") | .name' 2> /dev/null) && [[ -n "$jobs" ]]; then
      digest=$(LC_ALL=C sort <<< "$jobs" | sha256sum | cut -d' ' -f1)
    fi
  fi
  printf 'consumer_head=%s\nconsumer_jobs=%s\nreason=%s\n' "$head" "$digest" "$reason" >> "$GITHUB_OUTPUT"
  echo "CONSUMER_CANARY recorded run=${run_id:-none} head=${head:+measured} jobs=${digest:+measured} reason=${reason:-none}"
}

# The shared source selector owns pairing grammar. This legacy dispatcher
# projects that owner without inheriting credentials into the pure parser.
canary_read_pairing() {
  local text=$1 reader
  reader="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/consumer-pairing.mjs"
  CANARY_PAIRED=$(printf '%s' "$text" | env -i PATH="$PATH" node "$reader" "${CONSUMER_PULL_REQUEST:-}") || exit 1
}

canary_dispatch() {
  local repo=${CANARY_REPOSITORY:-} workflow=${CANARY_WORKFLOW:-}
  local revision=${SOURCE_REVISION:-} workspace_version=${WORKSPACE_VERSION:-}
  # The observation owner bounds queue wait separately from execution and
  # keeps the original child identity across credential renewals.
  # The job joins the owner and the secret's name, so an unset secret arrives
  # as "owner/" and must not reach the API as a half-formed repository.
  [[ "$repo" =~ ^[^/]+/[^/]+$ ]] \
    || canary_fail consumer_repository_unset "secret=CONSUMER_CANARY_REPOSITORY"
  CANARY_SECRET_NAME=${repo#*/}
  canary_say "CONSUMER_CANARY consumer=configured secret=CONSUMER_CANARY_REPOSITORY"
  [[ -n "$workflow" ]] || canary_fail consumer_workflow_unset
  [[ "$revision" =~ ^[0-9a-f]{40}$ ]] || canary_fail source_revision_invalid
  local producer_run=${CANARY_RELEASE_PRODUCER_RUN_ID:-}
  local producer_attempt=${CANARY_RELEASE_PRODUCER_RUN_ATTEMPT:-}
  local -a producer_inputs=()
  if [[ -n "$producer_run$producer_attempt" ]]; then
    [[ "$producer_run" =~ ^[1-9][0-9]*$ && "$producer_attempt" =~ ^[1-9][0-9]*$ \
      && "$producer_run" == "${GITHUB_RUN_ID:-}" && "$producer_attempt" == "${GITHUB_RUN_ATTEMPT:-}" \
      && "$revision" == "${GITHUB_SHA:-}" && "${GITHUB_REF:-}" == refs/heads/main \
      && -z "${CANARY_PULL_REQUEST_CLI_RUN:-}${CANARY_PULL_REQUEST_BASE:-}${REHEARSAL_PROMOTION_RUN_ID:-}" ]] \
      || canary_fail release_producer_identity_invalid
    producer_inputs=(-f release_producer_run_id="$producer_run" -f release_producer_run_attempt="$producer_attempt")
  fi
  # The consumer reads the target as a tag name, so resolve the source's
  # published version before choosing the dispatch target.
  local published_version target_version
  published_version="$(release_published_version_for_workspace "$workspace_version")" \
    || canary_fail workspace_version_unpublished "version=$workspace_version"
  target_version="v$published_version"

  local paired ref label=default
  canary_read_pairing "${PAIRING_TEXT:-}"
  paired=$CANARY_PAIRED
  CANARY_PAIRED_PULL=$paired
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

  if [[ -n "${REHEARSAL_PROMOTION_RUN_ID:-}" ]]; then
    [[ -z "$paired" ]] || canary_fail recovered_pairing_unsupported
    # shellcheck source=scripts/lib/release_consumer_verdict.sh
    source "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/release_consumer_verdict.sh"
    local reused
    reused="$(release_authenticated_successful_rehearsal "$GITHUB_REPOSITORY" \
      "$REHEARSAL_PROMOTION_RUN_ID" "${CANDIDATE_RUN_ID:-}" "$revision" "$repo")" \
      || canary_fail recovered_rehearsal_unqualified
    CANARY_RUN_ID=$reused
    CANARY_STARTED_AT=$(date +%s)
    canary_say "CONSUMER_CANARY reused run=$reused promotion=$REHEARSAL_PROMOTION_RUN_ID"
    return
  fi

  local observer started dispatched run_id
  observer=$(CANARY_REF="$ref" CANARY_TARGET="$target_version" \
    node "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/consumer-observer-dispatch.mjs" --admit) \
    || canary_fail dispatch_custody_unproven
  started=$(jq -er .started_at <<< "$observer") || canary_fail dispatch_custody_unproven
  # A pull request check names the CI run that built its CLI, so the consumer
  # installs that CLI instead of building the revision.
  local -a pull_request_inputs=()
  if [[ -n "${CANARY_PULL_REQUEST_CLI_RUN:-}" ]]; then
    [[ "$CANARY_PULL_REQUEST_CLI_RUN" =~ ^[1-9][0-9]*$ && "${CANARY_PULL_REQUEST_BASE:-}" =~ ^[0-9a-f]{40}$ ]] \
      || canary_fail pull_request_identity_invalid
    pull_request_inputs=(-f harn_cli_run="$CANARY_PULL_REQUEST_CLI_RUN" -f harn_base="$CANARY_PULL_REQUEST_BASE")
  fi
  # The base rehearsal installs the CLI the same run built at the base.
  case "${CANARY_PULL_REQUEST_CLI_ARTIFACT:-}" in
    "") ;;
    harn-base-cli.tar.zst)
      [[ -n "${CANARY_PULL_REQUEST_CLI_RUN:-}" && "$revision" == "${CANARY_PULL_REQUEST_BASE:-}" ]] \
        || canary_fail pull_request_identity_invalid
      pull_request_inputs+=(-f harn_cli_artifact="$CANARY_PULL_REQUEST_CLI_ARTIFACT")
      ;;
    *) canary_fail pull_request_identity_invalid ;;
  esac
  dispatched=$(gh workflow run "$workflow" -R "$repo" --ref "$ref" \
    -f target="$target_version" -f source_revision="$revision" -f legs=clean -f observer="$observer" \
    "${pull_request_inputs[@]}" "${producer_inputs[@]}" 2>&1) \
    || canary_fail dispatch_refused
  run_id=$(grep -oE "https://github\.com/$repo/actions/runs/[0-9]+" <<< "$dispatched" | head -1 || true)
  run_id=${run_id##*/}
  [[ -n "$run_id" ]] || canary_fail dispatch_returned_no_run
  canary_say "CONSUMER_CANARY dispatched run=$run_id ref=$label started_at=$started"

  CANARY_RUN_ID=$run_id
  CANARY_STARTED_AT=$started
}

# One credential owns at most 45 minutes of observation. Workflow windows
# renew the same scoped App authority and carry the original run and clock.
canary_observe() {
  local repo=${CANARY_REPOSITORY:-} run_id=${CANARY_RUN_ID:-} started=${CANARY_STARTED_AT:-}
  local policy poll_limit deadline_limit window_limit budget_version=1 budget_reader
  budget_reader="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/consumer-observer-budget.mjs"
  if [[ -n "${OBSERVER_CONTEXT:-}" ]]; then
    policy=$(node "$budget_reader" --policy) || canary_fail observation_budget_contract_invalid
    budget_version=2
  else
    # A reused completed rehearsal has no new declaration or child custody.
    [[ -n "${REHEARSAL_PROMOTION_RUN_ID:-}" ]] || canary_fail observation_identity_invalid
    policy=$(consumer_canary_policy) || canary_fail observation_budget_contract_invalid
  fi
  read -r poll_limit deadline_limit window_limit <<< "$(jq -r '[.poll_seconds,.deadline_seconds,.window_seconds] | @tsv' <<< "$policy")"
  local poll=${CANARY_POLL_SECONDS:-$poll_limit} deadline=${CANARY_DEADLINE_SECONDS:-$deadline_limit}
  local window=${CANARY_WINDOW_SECONDS:-$window_limit} window_started
  [[ "$repo" =~ ^[^/]+/[^/]+$ ]] || canary_fail consumer_repository_unset
  CANARY_SECRET_NAME=${repo#*/}
  [[ "$run_id" =~ ^[1-9][0-9]*$ && "$started" =~ ^[1-9][0-9]*$ ]] \
    || canary_fail observation_identity_invalid
  trap 'canary_fail observer_cancelled "run=$CANARY_RUN_ID verdict=unmeasured"' TERM INT
  [[ "$window" =~ ^[0-9]+$ && "$window" -le "$window_limit" ]] \
    || canary_fail observation_window_invalid "run=$run_id"
  [[ "$poll" =~ ^[0-9]+$ && "$poll" -le "$poll_limit" && "$deadline" =~ ^-?[0-9]+$ && "$deadline" -le "$deadline_limit" ]] \
    || canary_fail observation_budget_invalid "run=$run_id"
  window_started=$(date +%s)
  CANARY_PENDING=false

  local status conclusion now record timing reason
  while true; do
    CANARY_TIMING_RECEIPT=
    now=$(date +%s)
    ((now >= started)) || canary_fail observation_clock_invalid "run=$run_id"
    if [[ "$budget_version" == 1 ]] && ((now - started >= deadline)); then
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
    if [[ "$budget_version" == 2 ]]; then
      timing=$(CANARY_REPOSITORY="$repo" CANARY_RUN_ID="$run_id" node "$budget_reader") \
        || canary_fail job_timing_unmeasured "run=$run_id verdict=unmeasured"
      CANARY_TIMING_RECEIPT=$timing
      reason=$(jq -er '.reason // "none"' <<< "$timing") \
        || canary_fail job_timing_unmeasured "run=$run_id verdict=unmeasured"
      if [[ "$reason" != none ]]; then
        canary_fail "$reason" "run=$run_id verdict=unmeasured wall_seconds=$((now - started))"
      fi
      if [[ "$status" == completed ]] && ! jq -e '.owned == 1' <<< "$timing" >/dev/null; then
        canary_fail job_timing_custody_unmeasured "run=$run_id verdict=unmeasured"
      fi
      # Even an expired renewal reads the original child's fresh authenticated
      # census before classifying its deadline. A shorter caller limit may tighten it.
      if ((now - started >= deadline)); then
        canary_fail aggregate_deadline "run=$run_id verdict=unmeasured wall_seconds=$((now - started))"
      fi
    fi
    [[ "$status" == completed ]] && break
    if [[ -n "${CANARY_PAIRED_PULL:-}" ]]; then
      local paired_state paired_repository _paired_ref
      record=$(gh api "repos/$repo/pulls/$CANARY_PAIRED_PULL" \
        --jq '"\(.state) \(.head.repo.full_name // "none") \(.head.ref)"' 2> /dev/null) \
        || canary_fail paired_pull_unreadable "verdict=unmeasured"
      read -r paired_state paired_repository _paired_ref <<< "$record"
      [[ "$paired_state" == open && "$paired_repository" == "$repo" ]] \
        || canary_fail paired_pull_no_longer_needed "verdict=unmeasured"
    fi
    if ((now - window_started >= window)); then
      CANARY_PENDING=true
      if [[ -n "$CANARY_TIMING_RECEIPT" ]]; then
        canary_say "CONSUMER_CANARY timing=$CANARY_TIMING_RECEIPT"
      fi
      canary_say "CONSUMER_CANARY pending run=$run_id status=$status wall_seconds=$((now - started))"
      return 0
    fi
    sleep "$poll"
  done

  local verdict=fail
  if [[ -n "$CANARY_TIMING_RECEIPT" ]]; then
    canary_say "CONSUMER_CANARY timing=$CANARY_TIMING_RECEIPT"
  fi
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
  [[ -n "$repo" ]] || canary_fail repository_unset
  [[ -n "$output" ]] || canary_fail output_unset
  if [[ -z "$sha" ]]; then
    sha=$(green_tip "$repo" 2> /dev/null) || canary_fail green_unreadable
    [[ -n "$sha" ]] || canary_fail green_absent
  fi
  [[ "$sha" =~ ^[0-9a-f]{40}$ ]] || canary_fail source_revision_invalid
  if [[ "$event" != schedule ]]; then
    canary_say "CONSUMER_CANARY run reason=explicit_$event main=$sha"
    printf 'run=true\nsource_revision=%s\n' "$sha" >> "$output"
    return 0
  fi
  local history id head settled last='' last_run=''
  history=$(gh api "repos/$repo/actions/workflows/consumer-canary.yml/runs?branch=main&status=completed&per_page=20" \
    --jq '.workflow_runs[] | "\(.id) \(.head_sha)"' 2> /dev/null) \
    || canary_fail settled_history_unreadable
  while read -r id head; do
    [[ -n "$id" && "$id" != "$current" ]] || continue
    # The settled step names the revision it rehearsed; an unnamed one rehearsed the run's head.
    settled=$(gh api "repos/$repo/actions/runs/$id/jobs" \
      --jq '[.jobs[] | select(.name == "Settled verdict" and .conclusion == "success")
        | ([.steps[]?.name | capture("^Record the settled verdict at (?<sha>[0-9a-f]{40})$").sha] | first) // "head"]
        | first // ""' \
      2> /dev/null) || canary_fail settled_history_unreadable "run=$id"
    case "$settled" in
      "") continue ;;
      head) last=$head ;;
      *) [[ "$settled" =~ ^[0-9a-f]{40}$ ]] || canary_fail settled_history_unreadable "run=$id"; last=$settled ;;
    esac
    last_run=$id
    break
  done <<< "$history"
  if [[ -z "$last" ]]; then
    canary_say "CONSUMER_CANARY run reason=no_settled_verdict main=$sha"
    printf 'run=true\nsource_revision=%s\n' "$sha" >> "$output"
  elif [[ "$last" == "$sha" ]]; then
    canary_say "CONSUMER_CANARY skipped reason=main_unchanged main=$sha last_settled=$last last_settled_run=$last_run"
    printf 'run=false\nsource_revision=%s\n' "$sha" >> "$output"
  else
    canary_say "CONSUMER_CANARY run reason=main_moved main=$sha last_settled=$last last_settled_run=$last_run"
    printf 'run=true\nsource_revision=%s\n' "$sha" >> "$output"
  fi
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  case "${1:-}" in
    --decide) canary_decide ;;
    --dispatch)
      CANARY_DISPATCHING=true
      canary_dispatch
      [[ -n "${GITHUB_OUTPUT:-}" ]] || canary_fail observation_output_unset
      printf 'run_id=%s\nstarted_at=%s\npaired_pull=%s\n' "$CANARY_RUN_ID" "$CANARY_STARTED_AT" "${CANARY_PAIRED_PULL:-}" >> "$GITHUB_OUTPUT"
      ;;
    --observe) canary_observe ;;
    --record) canary_record ;;
    "") canary_main ;;
    *) canary_fail invocation_unrecognized ;;
  esac
fi
