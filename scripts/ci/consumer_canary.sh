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

# Prints one response header's value from a `gh api --include` header block.
canary_header() {
  awk -v name="$1" '{ sub(/\r$/, "") } tolower($1) == tolower(name) ":" { print $2; exit }' <<< "$2"
}

# Reads one GitHub API resource with the step's GH_TOKEN into the variable
# named by the first argument; the rest are `gh api` arguments after the
# caller's reason and the repository's printable label. `--optional` returns 1
# instead of failing; `--detail TEXT` appends the caller's context to a failure.
# A rate limit, a server error or a dropped connection is retried with bounded
# backoff; a denial fails at once. A failure names its class, the HTTP status,
# the token's step label and the repository's label, never a token, a response
# body or a private name:
#
#   api_access_denied  401, 404, or a 403 that is not a rate limit.
#   api_rate_limited   429, or 403 with an exhausted or secondary rate limit.
#   api_unavailable    5xx or no HTTP response.
#   <reason>           any other status, under the caller's own reason.
#
# Locals are prefixed because the result is assigned through dynamic scope.
canary_api() {
  CANARY_API_FAILURE_CLASS=
  CANARY_API_FAILURE_DETAIL=
  CANARY_API_RETRY_WAIT_SECONDS=0
  local _optional=false _context=''
  while true; do
    case "$1" in
      --optional) _optional=true; shift ;;
      --detail) _context=" $2"; shift 2 ;;
      *) break ;;
    esac
  done
  local _into=$1 _reason=$2 _repository=$3
  shift 3
  local _attempts=${CANARY_API_ATTEMPTS:-4} _backoff=${CANARY_API_BACKOFF_SECONDS:-5}
  local _cap=${CANARY_API_WAIT_LIMIT_SECONDS:-300} _attempt=1 _final
  local _response _headers _body _status _class _wait _remaining _retry_after _reset _message _detail
  while true; do
    _response=$(gh api --include "$@" 2> /dev/null) || true
    _headers=$(awk '{ sub(/\r$/, "") } /^$/ { exit } { print }' <<< "$_response")
    _body=$(awk 'seen { print } /^\r?$/ { seen = 1 }' <<< "$_response")
    _status=$(sed -nE '1s/^HTTP\/[0-9.]+ ([0-9]{3}).*/\1/p' <<< "$_headers")
    if [[ "$_status" == 2?? ]]; then
      printf -v "$_into" '%s' "$_body"
      return 0
    fi
    _remaining=$(canary_header x-ratelimit-remaining "$_headers")
    _retry_after=$(canary_header retry-after "$_headers")
    _reset=$(canary_header x-ratelimit-reset "$_headers")
    _message=$(jq -r '.message // empty' <<< "$_body" 2> /dev/null || true)
    _wait=$((_backoff * (1 << (_attempt - 1))))
    _final=false
    case "$_status" in
      ""|5??) _class=api_unavailable ;;
      429) _class=api_rate_limited ;;
      403)
        if [[ "$_remaining" == 0 || -n "$_retry_after" || "$_message" == *"rate limit"* ]]; then
          _class=api_rate_limited
        else
          _class=api_access_denied _final=true
        fi
        ;;
      401|404) _class=api_access_denied _final=true ;;
      *) _class=$_reason _final=true ;;
    esac
    if [[ "$_class" == api_rate_limited ]]; then
      if [[ "$_retry_after" =~ ^[0-9]+$ ]]; then
        _wait=$_retry_after
      elif [[ "$_remaining" == 0 && "$_reset" =~ ^[0-9]+$ ]]; then
        _wait=$((_reset - $(date +%s) + 1))
        ((_wait > 0)) || _wait=1
      fi
    fi
    if [[ "$_final" == true ]] || ((_attempt >= _attempts || _wait > _cap)); then
      _detail="read=$_reason status=${_status:-none} token=${CANARY_TOKEN_LABEL:-GH_TOKEN} repository=$_repository attempts=$_attempt"
      [[ -z "$_remaining" ]] || _detail+=" ratelimit_remaining=$_remaining"
      _detail+=$_context
      if [[ "$_optional" == true ]]; then
        CANARY_API_FAILURE_CLASS=$_class
        CANARY_API_FAILURE_DETAIL=$_detail
        CANARY_API_RETRY_WAIT_SECONDS=$_wait
        canary_say "CONSUMER_CANARY unread=$_class $_detail"
        return 1
      fi
      canary_fail "$_class" "$_detail"
    fi
    canary_say "CONSUMER_CANARY retry=$_class read=$_reason status=${_status:-none} attempt=$_attempt wait_seconds=$_wait"
    sleep "$_wait"
    _attempt=$((_attempt + 1))
  done
}

canary_observation_checkpoint() {
  [[ -n "${CANARY_CHECKPOINT_FILE:-}" && "${CANARY_OBSERVATION_INITIALIZED:-false}" == true ]] || return 0
  local state=$1 reason=$2 next=$3 temporary now
  now=$(date +%s)
  mkdir -p "$(dirname "$CANARY_CHECKPOINT_FILE")"
  temporary=$(mktemp "${CANARY_CHECKPOINT_FILE}.XXXXXX")
  jq -n --arg state "$state" --arg reason "$reason" --arg next "$next" \
    --arg run "$CANARY_RUN_ID" --arg parent "${GITHUB_RUN_ID:-}" --arg attempt "${GITHUB_RUN_ATTEMPT:-}" \
    --arg context "${OBSERVER_CONTEXT:-}" --argjson started "$CANARY_STARTED_AT" \
    --argjson observed "$now" --argjson deadline "$CANARY_OBSERVATION_DEADLINE" \
    --argjson not_before "${CANARY_READ_NOT_BEFORE:-0}" --argjson timing "${CANARY_TIMING_RECEIPT:-null}" \
    '{schema:"harn.consumer_observer.checkpoint.v1",stage:"consumer-observation",state:$state,
      parent_run_id:$parent,parent_run_attempt:$attempt,run_id:$run,started_at:$started,
      observed_at:$observed,deadline_at:($started+$deadline),read_not_before:$not_before,reason:$reason,next_action:$next,
      source_sha:((try ($context|fromjson|.source_revision) catch null) |
        if type=="string" and test("^[0-9a-f]{40}$") then . else null end),timing:$timing}' > "$temporary"
  mv -- "$temporary" "$CANARY_CHECKPOINT_FILE"
  if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    echo "checkpoint=$CANARY_CHECKPOINT_FILE" >> "$GITHUB_OUTPUT"
  fi
}

canary_fail() {
  canary_observation_checkpoint "${3:-unmeasured}" "$1" inspect_same_run
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
  local reason=${CANARY_DISPATCH_REASON:-} head='' jobs digest='' failed failures=''
  [[ -n "${GITHUB_OUTPUT:-}" ]] || { echo '::error::CONSUMER_CANARY reason=record_output_unset'; exit 1; }
  if [[ -z "$reason" && -n "$receipt" && -f "$receipt" ]]; then
    reason=$(jq -r '.reason // empty' "$receipt" 2> /dev/null || true)
  fi
  [[ "$reason" =~ ^[a-z_]*$ ]] || reason=failure_receipt_unreadable
  if [[ "$repo" =~ ^[^/]+/[^/]+$ && "$run_id" =~ ^[1-9][0-9]*$ ]]; then
    CANARY_SECRET_NAME=${repo#*/}
    canary_api --optional head consumer_run_head secret:CONSUMER_CANARY_REPOSITORY \
      "repos/$repo/actions/runs/$run_id" --jq .head_sha || head=''
    [[ "$head" =~ ^[0-9a-f]{40}$ ]] || head=''
    # A pinned rehearsal measured its pin only if every leg that checked out
    # source proved the pin; the dispatched ref's head is not what it measured.
    if [[ -n "${CANARY_CONSUMER_REVISION:-}" ]]; then
      local pinned=''
      canary_api --optional pinned consumer_run_pin secret:CONSUMER_CANARY_REPOSITORY \
        "repos/$repo/actions/runs/$run_id/jobs?filter=latest&per_page=100" \
        --jq 'if .total_count <= 100 then [.jobs[] | .steps[]? | select(.name == "Verify the pinned consumer revision") | .conclusion] else [] end
          | if length > 0 and all(. == "success") then "proven" else "" end' || pinned=''
      if [[ "$pinned" == proven && "$CANARY_CONSUMER_REVISION" =~ ^[0-9a-f]{40}$ ]]; then
        head=$CANARY_CONSUMER_REVISION
      else
        head=''
      fi
    fi
    # One page holds every job; a larger run leaves the digest unmeasured.
    if canary_api --optional jobs consumer_run_jobs secret:CONSUMER_CANARY_REPOSITORY \
        "repos/$repo/actions/runs/$run_id/jobs?filter=latest&per_page=100" \
        --jq 'if .total_count <= 100 then .jobs[] | select(.conclusion != "skipped") | .name else empty end' \
        && [[ -n "$jobs" ]]; then
      digest=$(LC_ALL=C sort <<< "$jobs" | sha256sum | cut -d' ' -f1)
    fi
    # Where the run failed, so a base excuses a red only by failing the same way.
    if canary_api --optional failed consumer_run_failures secret:CONSUMER_CANARY_REPOSITORY \
        "repos/$repo/actions/runs/$run_id/jobs?filter=latest&per_page=100" \
        --jq 'if .total_count <= 100 then .jobs[] | select(.conclusion == "failure") | "\(.name)\t\([.steps[]? | select(.conclusion == "failure") | .name][0] // "")" else empty end' \
        && [[ -n "$failed" ]]; then
      failures=$(LC_ALL=C sort <<< "$failed" | sha256sum | cut -d' ' -f1)
    fi
  fi
  printf 'consumer_head=%s\nconsumer_jobs=%s\nconsumer_failures=%s\nreason=%s\n' "$head" "$digest" "$failures" "$reason" >> "$GITHUB_OUTPUT"
  echo "CONSUMER_CANARY recorded run=${run_id:-none} head=${head:+measured} jobs=${digest:+measured} failures=${failures:+measured} reason=${reason:-none}"
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
      && "${GITHUB_REF:-}" == refs/heads/main \
      && -z "${CANARY_PULL_REQUEST_CLI_RUN:-}${CANARY_PULL_REQUEST_BASE:-}${REHEARSAL_PROMOTION_RUN_ID:-}" ]] \
      || canary_fail release_producer_identity_invalid
    if [[ "$producer_run" == "${GITHUB_RUN_ID:-}" && "$producer_attempt" == "${GITHUB_RUN_ATTEMPT:-}" \
      && "$revision" == "${GITHUB_SHA:-}" ]]; then
      :
    else
      [[ "${CONSUMER_PULL_REQUEST:-}" =~ ^[1-9][0-9]*$ ]] || canary_fail release_producer_identity_invalid
      node "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/consumer-observer-dispatch.mjs" --archive-producer >/dev/null \
        || canary_fail release_producer_identity_invalid
    fi
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
    canary_api --detail "pull=$paired" pull paired_pull_unreadable \
      secret:CONSUMER_CANARY_REPOSITORY "repos/$repo/pulls/$paired" --jq '"\(.state) \(.head.repo.full_name // "none") \(.head.ref)"'
    read -r state head_repo ref <<< "$pull"
    [[ "$state" == open ]] || canary_fail paired_pull_not_open "pull=$paired state=$state"
    [[ "$head_repo" == "$repo" ]] || canary_fail paired_pull_from_fork "pull=$paired"
  else
    canary_api ref consumer_unreadable secret:CONSUMER_CANARY_REPOSITORY "repos/$repo" --jq .default_branch
    [[ -n "$ref" ]] || canary_fail consumer_unreadable "read=consumer_default_branch"
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
    # Both rehearsals of one pull request measure the one consumer commit its run pinned.
    [[ "${CANARY_CONSUMER_REVISION:-}" =~ ^[0-9a-f]{40}$ ]] || canary_fail consumer_revision_invalid
    pull_request_inputs+=(-f consumer_revision="$CANARY_CONSUMER_REVISION")
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
  # GitHub rejects a rate-limited dispatch before acting on it, so only that
  # refusal is retried; any other failure may have dispatched and is final.
  local attempt=1 status wait
  until dispatched=$(gh workflow run "$workflow" -R "$repo" --ref "$ref" \
    -f target="$target_version" -f source_revision="$revision" -f legs=clean -f observer="$observer" \
    "${pull_request_inputs[@]}" "${producer_inputs[@]}" 2>&1); do
    status=$(grep -oE 'HTTP [0-9]{3}' <<< "$dispatched" | head -1 | cut -d' ' -f2 || true)
    wait=$((${CANARY_API_BACKOFF_SECONDS:-5} * 3 * (1 << (attempt - 1))))
    if [[ ! ("$status" == 429 || ("$status" == 403 && "$dispatched" == *"rate limit"*)) ]] \
        || ((attempt >= ${CANARY_API_ATTEMPTS:-4})); then
      canary_fail dispatch_refused "status=${status:-none} token=${CANARY_TOKEN_LABEL:-GH_TOKEN} repository=secret:CONSUMER_CANARY_REPOSITORY attempts=$attempt"
    fi
    canary_say "CONSUMER_CANARY retry=api_rate_limited read=dispatch status=$status attempt=$attempt wait_seconds=$wait"
    sleep "$wait"
    attempt=$((attempt + 1))
  done
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
  CANARY_OBSERVATION_INITIALIZED=true
  CANARY_OBSERVATION_DEADLINE=$deadline
  CANARY_READ_NOT_BEFORE=0
  if [[ -n "${CANARY_CHECKPOINT_FILE:-}" && -f "$CANARY_CHECKPOINT_FILE" ]]; then
    CANARY_READ_NOT_BEFORE=$(jq -er --arg run "$run_id" --arg parent "${GITHUB_RUN_ID:-}" \
      --arg attempt "${GITHUB_RUN_ATTEMPT:-}" --argjson started "$started" --argjson deadline "$deadline" '
      select(.schema == "harn.consumer_observer.checkpoint.v1" and .run_id == $run
        and .parent_run_id == $parent and .parent_run_attempt == $attempt
        and .started_at == $started and .deadline_at == ($started+$deadline)) |
      .read_not_before | select(type == "number" and . >= 0 and . <= 9007199254740991 and floor == .)' "$CANARY_CHECKPOINT_FILE") || CANARY_READ_NOT_BEFORE=0
  fi
  CANARY_TIMING_RECEIPT=
  canary_observation_checkpoint pending none observe_same_run

  local status conclusion now record timing reason read_error retry_count=0 retry_wait remaining advice
  while true; do
    CANARY_TIMING_RECEIPT=
    now=$(date +%s)
    ((now >= started)) || canary_fail observation_clock_invalid "run=$run_id"
    if (( CANARY_READ_NOT_BEFORE > 0 && now - started >= deadline )); then
      canary_fail aggregate_deadline "run=$run_id verdict=unmeasured wall_seconds=$((now - started))"
    fi
    if [[ "$budget_version" == 1 ]] && ((now - started >= deadline)); then
      canary_fail no_verdict_before_deadline "run=$run_id verdict=unmeasured wall_seconds=$((now - started))"
    fi
    read_error=
    advice=0
    if (( CANARY_READ_NOT_BEFORE > now )); then
      read_error=api_backoff_pending
    elif canary_api --optional --detail "run=$run_id verdict=unmeasured" record consumer_read_refused \
      secret:CONSUMER_CANARY_REPOSITORY "repos/$repo/actions/runs/$run_id" --jq '"\(.status) \(.conclusion // "none")"'; then
      read -r status conclusion <<< "$record"
      case "$status" in
        completed|queued|in_progress|waiting|pending|requested) ;;
        *) canary_fail consumer_state_unreported "run=$run_id verdict=unmeasured" ;;
      esac
    else
      case "$CANARY_API_FAILURE_CLASS" in
        api_unavailable|api_rate_limited) read_error=$CANARY_API_FAILURE_CLASS; advice=$CANARY_API_RETRY_WAIT_SECONDS ;;
        *) canary_fail "$CANARY_API_FAILURE_CLASS" "$CANARY_API_FAILURE_DETAIL" ;;
      esac
    fi
    if [[ -z "$read_error" && "$budget_version" == 2 ]]; then
      if timing=$(CANARY_REPOSITORY="$repo" CANARY_RUN_ID="$run_id" node "$budget_reader"); then
        :
      elif jq -e '.schema == "harn.consumer_observer.read_refusal.v1" and .retryable == true' <<< "$timing" >/dev/null; then
        read_error=$(jq -er .reason <<< "$timing")
        advice=$(jq -er '.retry_after_seconds | select(type == "number" and . >= 0 and floor == .)' <<< "$timing") \
          || canary_fail job_timing_unmeasured "run=$run_id verdict=unmeasured"
      else
        canary_fail job_timing_unmeasured "run=$run_id verdict=unmeasured"
      fi
    fi
    if [[ -n "$read_error" ]]; then
      now=$(date +%s)
      if (( advice > 0 )); then CANARY_READ_NOT_BEFORE=$((now + advice)); fi
      canary_observation_checkpoint pending "$read_error" retry_same_run
      if (( now - started >= deadline )); then
        canary_fail aggregate_deadline "run=$run_id verdict=unmeasured wall_seconds=$((now - started))"
      fi
      if (( now - window_started >= window )); then
        CANARY_PENDING=true
        canary_observation_checkpoint pending "$read_error" renew_same_run
        canary_say "CONSUMER_CANARY pending run=$run_id status=unmeasured wall_seconds=$((now - started)) next_action=renew_same_run"
        return 0
      fi
      retry_wait=$((5 << (retry_count < 4 ? retry_count : 4)))
      (( retry_wait <= poll_limit )) || retry_wait=$poll_limit
      remaining=$((CANARY_READ_NOT_BEFORE - now))
      (( remaining <= retry_wait )) || retry_wait=$remaining
      remaining=$((deadline - now + started))
      (( retry_wait <= remaining )) || retry_wait=$remaining
      remaining=$((window - now + window_started))
      (( retry_wait <= remaining )) || retry_wait=$remaining
      canary_say "CONSUMER_CANARY retry=$read_error run=$run_id wait_seconds=$retry_wait next_action=retry_same_run"
      sleep "$retry_wait"
      retry_count=$((retry_count + 1))
      continue
    fi
    retry_count=0
    CANARY_READ_NOT_BEFORE=0
    if [[ "$budget_version" == 2 ]]; then
      CANARY_TIMING_RECEIPT=$timing
      canary_observation_checkpoint pending none observe_same_run
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
      canary_api --detail "verdict=unmeasured" record paired_pull_unreadable \
        secret:CONSUMER_CANARY_REPOSITORY "repos/$repo/pulls/$CANARY_PAIRED_PULL" --jq '"\(.state) \(.head.repo.full_name // "none") \(.head.ref)"'
      read -r paired_state paired_repository _paired_ref <<< "$record"
      [[ "$paired_state" == open && "$paired_repository" == "$repo" ]] \
        || canary_fail paired_pull_no_longer_needed "verdict=unmeasured"
    fi
    if ((now - window_started >= window)); then
      CANARY_PENDING=true
      canary_observation_checkpoint pending none renew_same_run
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
  canary_observation_checkpoint "$verdict" none "$([[ "$verdict" == pass ]] && echo none || echo inspect_same_run)"
  if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    echo "verdict=$verdict" >> "$GITHUB_OUTPUT"
  fi
  canary_say "CONSUMER_CANARY verdict=$verdict conclusion=$conclusion run=$run_id wall_seconds=$((now - started))"
  [[ "$verdict" == pass ]] || canary_fail consumer_rehearsal_failed "run=$run_id conclusion=$conclusion" fail
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
  canary_api history settled_history_unreadable "$repo" \
    "repos/$repo/actions/workflows/consumer-canary.yml/runs?branch=main&status=completed&per_page=20" \
    --jq '.workflow_runs[] | "\(.id) \(.head_sha)"'
  while read -r id head; do
    [[ -n "$id" && "$id" != "$current" ]] || continue
    # The settled step names the revision it rehearsed; an unnamed one rehearsed the run's head.
    canary_api --detail "run=$id" settled settled_history_unreadable "$repo" "repos/$repo/actions/runs/$id/jobs" \
      --jq '[.jobs[] | select(.name == "Settled verdict" and .conclusion == "success")
        | ([.steps[]?.name | capture("^Record the settled verdict at (?<sha>[0-9a-f]{40})$").sha] | first) // "head"]
        | first // ""'
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
