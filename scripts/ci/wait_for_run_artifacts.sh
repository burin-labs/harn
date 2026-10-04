#!/usr/bin/env bash
# Wait for immutable v4 artifacts published earlier in this workflow run, or in
# the run HARN_EXT_ARTIFACT_RUN_ID and HARN_EXT_ARTIFACT_RUN_ATTEMPT name.
#
# GitHub exposes v4 artifacts through the REST API as soon as their upload step
# completes, but job dependencies are terminal-state barriers. This bounded
# bootstrap wait lets free hosted consumers overlap the producer's test phase
# without weakening the producer's fail-closed result.
#
# The wait spends the workflow token's API budget, which every job in the
# repository shares, and a run starts about ten of these waits at once. On
# 2026-10-02 a slow producer kept ten of them reading two endpoints every 15s
# for 25 minutes, and the installation limit failed all ten together (run
# 36987728246). So the inventory is read as a conditional request, which
# GitHub answers with a 304 that the limit does not count while nothing has
# been uploaded, and the producer's state, which changes with every job in the
# run and so always costs a request, is read on only every Nth poll once the
# producer is running. The wait reads only the producer's state until the
# producer has started (no artifact can exist before then), backs off
# exponentially, gives up by name when the producer never starts, and waits
# out a rate limit for as long as the refusing response itself says, never
# guessing.
#
# A producer that never leaves the queue exits 3, not 1, with a
# "Producer never started" error annotation that names the runner labels it
# queued on. Starvation is a capacity fact about a runner pool, not a defect in
# the commit, and on 2026-10-02 ten consumers reported it only in their own
# logs while the run's verdict named an unrelated lane (run 37042288569).
set -euo pipefail

# The exit status reserved for a producer that never started.
producer_never_started_status=3

if [ "$#" -eq 0 ]; then
  echo "usage: $0 ARTIFACT [ARTIFACT ...]" >&2
  exit 2
fi

repository="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY must name owner/repo}"
# A main push already proven by the merge group for its exact commit reads that
# run's artifacts instead of rebuilding them. Both values come from the proof,
# so they are named together or not at all.
if [ -n "${HARN_EXT_ARTIFACT_RUN_ID:-}" ] || [ -n "${HARN_EXT_ARTIFACT_RUN_ATTEMPT:-}" ]; then
  run_id="${HARN_EXT_ARTIFACT_RUN_ID:-}"
  run_attempt="${HARN_EXT_ARTIFACT_RUN_ATTEMPT:-}"
  if [ -z "$run_id" ] || [ -z "$run_attempt" ]; then
    echo "HARN_EXT_ARTIFACT_RUN_ATTEMPT must accompany HARN_EXT_ARTIFACT_RUN_ID" >&2
    exit 2
  fi
  case "$run_id" in
    ''|*[!0-9]*) echo "HARN_EXT_ARTIFACT_RUN_ID must be a workflow run id" >&2; exit 2 ;;
  esac
  echo "reading artifacts from workflow run ${run_id} attempt ${run_attempt}"
else
  run_id="${GITHUB_RUN_ID:?GITHUB_RUN_ID must identify the current workflow run}"
  run_attempt="${GITHUB_RUN_ATTEMPT:?GITHUB_RUN_ATTEMPT must identify the current attempt}"
fi
producer_job="${HARN_EXT_ARTIFACT_PRODUCER_JOB:?HARN_EXT_ARTIFACT_PRODUCER_JOB must name the producing job}"
# Retained for callers using the existing setting: this bounds consecutive
# unreadable producer-state observations, not time spent in a measured queue.
max_unmeasured_attempts="${HARN_EXT_ARTIFACT_WAIT_MAX_ATTEMPTS:-66}"
interval_seconds="${HARN_EXT_ARTIFACT_WAIT_INTERVAL_SECONDS:-10}"
# A rate-limited API is waited out rather than counted as unmeasured, up to
# this many seconds in total across the whole wait. The default fits inside
# the shortest job that runs this script; a reset further away fails at once,
# naming it, rather than as a job timeout that names nothing.
max_rate_limit_seconds="${HARN_EXT_ARTIFACT_WAIT_RATE_LIMIT_MAX_SECONDS:-240}"
# The poll interval doubles after each read that finds nothing new, up to this.
max_interval_seconds="${HARN_EXT_ARTIFACT_WAIT_MAX_INTERVAL_SECONDS:-60}"
# A producer still queued after this long is starved, not slow.
max_queue_seconds="${HARN_EXT_ARTIFACT_WAIT_MAX_QUEUE_SECONDS:-1800}"
# Once the producer is running, its state is re-read on every Nth poll only.
# The inventory read on the polls between is conditional and normally free;
# the state read is what tells a producer that finished without the artifact
# from one still building, so it is paced rather than dropped.
state_every_polls="${HARN_EXT_ARTIFACT_WAIT_STATE_EVERY_POLLS:-1}"

case "$run_attempt" in
  ''|*[!0-9]*|0) echo "GITHUB_RUN_ATTEMPT must be a positive integer" >&2; exit 2 ;;
esac
case "$max_unmeasured_attempts" in
  ''|*[!0-9]*|0) echo "HARN_EXT_ARTIFACT_WAIT_MAX_ATTEMPTS must be a positive integer" >&2; exit 2 ;;
esac
case "$interval_seconds" in
  ''|*[!0-9]*) echo "HARN_EXT_ARTIFACT_WAIT_INTERVAL_SECONDS must be a non-negative integer" >&2; exit 2 ;;
esac
case "$max_rate_limit_seconds" in
  ''|*[!0-9]*) echo "HARN_EXT_ARTIFACT_WAIT_RATE_LIMIT_MAX_SECONDS must be a non-negative integer" >&2; exit 2 ;;
esac
case "$max_interval_seconds" in
  ''|*[!0-9]*) echo "HARN_EXT_ARTIFACT_WAIT_MAX_INTERVAL_SECONDS must be a non-negative integer" >&2; exit 2 ;;
esac
case "$max_queue_seconds" in
  ''|*[!0-9]*) echo "HARN_EXT_ARTIFACT_WAIT_MAX_QUEUE_SECONDS must be a non-negative integer" >&2; exit 2 ;;
esac
case "$state_every_polls" in
  ''|*[!0-9]*|0) echo "HARN_EXT_ARTIFACT_WAIT_STATE_EVERY_POLLS must be a positive integer" >&2; exit 2 ;;
esac

artifacts=("$@")
for artifact in "${artifacts[@]}"; do
  case "$artifact" in
    ''|*[!A-Za-z0-9._-]*) echo "invalid artifact name: $artifact" >&2; exit 2 ;;
  esac
done

api_path="/repos/${repository}/actions/runs/${run_id}/artifacts?per_page=100"
jobs_path="/repos/${repository}/actions/runs/${run_id}/attempts/${run_attempt}/jobs?per_page=100"

# The last failed API read of this poll, and whether GitHub refused it for the
# rate limit. A refusal is not an observation of the producer, so the loop
# waits for the reset instead of spending the unmeasured-poll budget on it.
api_error=""
rate_limited=0
rate_limited_path=""
error_file=$(mktemp)
page_file=$(mktemp)
trap 'rm -f "$error_file" "$page_file"' EXIT

# Read every page of an API path into `pages`. Runs in this shell, not a
# command substitution, so the error and the rate-limit flag reach the loop.
gh_read() {
  if gh api "$1" --paginate --slurp > "$page_file" 2> "$error_file"; then
    pages=$(< "$page_file")
    return 0
  fi
  api_error=$(head -n 3 "$error_file" | tr '\n' ' ')
  if grep -qi 'rate limit' "$error_file"; then
    rate_limited=1
    rate_limited_path=$1
  fi
  return 1
}

# The inventory's last ETag and the names it listed. A 304 against that ETag
# means the same names, read without spending the limit.
artifacts_etag=""
artifact_names=""
# The headers of the last refused inventory read, which name its own reset.
refused_headers=""

# One conditional read of the inventory's first page. Sets `artifact_names`
# and returns 0 on a 200 that holds the whole inventory or on a 304; returns
# 2 when the inventory spans pages, and 1 when the read failed.
read_artifacts_conditionally() {
  local response status body headers
  local -a request=(api -i)
  if [[ -n $artifacts_etag ]]; then
    request+=(-H "If-None-Match: ${artifacts_etag}")
  fi
  request+=("$api_path")
  response=$(gh "${request[@]}" 2> "$error_file" | tr -d '\r') || true
  status=$(sed -n '1s/^HTTP\/[0-9.]* \([0-9][0-9]*\).*/\1/p' <<< "$response")
  headers=$(sed '/^$/q' <<< "$response")
  case "$status" in
    304)
      [[ -n $artifacts_etag ]] || return 1
      return 0
      ;;
    200)
      body=$(sed '1,/^$/d' <<< "$response")
      if ! jq -e '(.total_count | type) == "number" and (.artifacts | type) == "array"' \
        <<< "$body" > /dev/null 2>&1; then
        api_error="unreadable artifact inventory"
        return 1
      fi
      if ! jq -e '.total_count <= (.artifacts | length)' <<< "$body" > /dev/null; then
        return 2
      fi
      if ! artifact_names=$(jq -er '
        .artifacts
        | map(if (.name | type) != "string" or (.expired | type) != "boolean"
              then error("invalid artifact") else . end)
        | map(select(.expired == false) | .name) | join("\n")
      ' <<< "$body" 2>/dev/null); then
        api_error="invalid artifact in inventory"
        artifacts_etag=""
        return 1
      fi
      artifacts_etag=$(sed -n 's/^[Ee][Tt]ag:[[:space:]]*//p' <<< "$headers" | head -n 1)
      return 0
      ;;
  esac
  api_error=$(head -n 3 "$error_file" | tr '\n' ' ')
  if grep -qi 'rate limit' "$error_file" || grep -qi 'rate limit' <<< "$response"; then
    rate_limited=1
    rate_limited_path=$api_path
    refused_headers=$headers
  fi
  return 1
}

# An unreadable page is uncertainty, never an empty inventory or completion.
read_artifacts() {
  local pages names read_status=0
  missing=("${artifacts[@]}")
  read_artifacts_conditionally || read_status=$?
  if [ "$read_status" -eq 1 ]; then
    return 1
  fi
  names=$artifact_names
  if [ "$read_status" -eq 2 ]; then
    # More artifacts than one page holds: read every page, unconditionally.
    artifacts_etag=""
    if ! gh_read "$api_path"; then
      return 1
    fi
    if ! names=$(jq -er '
      if type != "array" or length == 0 then error("missing artifact pages") else . end
      | map(if (.artifacts | type) != "array" then error("invalid artifact page") else .artifacts end)
      | add
      | map(if (.name | type) != "string" or (.expired | type) != "boolean"
            then error("invalid artifact") else . end)
      | map(select(.expired == false) | .name) | join("\n")
    ' <<< "$pages" 2>/dev/null); then
      return 1
    fi
  fi
  missing=()
  for artifact in "${artifacts[@]}"; do
    if ! grep -Fxq "$artifact" <<< "$names"; then
      missing+=("$artifact")
    fi
  done
  [ "${#missing[@]}" -eq 0 ]
}

# Sets `state` to the producer's status. Runs in this shell, like gh_read, so a
# rate-limited read reaches the loop.
read_producer_state() {
  local pages
  gh_read "$jobs_path" || return 1
  state=$(jq -er --arg name "$producer_job" '
    if type != "array" or length == 0 then error("missing job pages") else . end
    | map(if (.jobs | type) != "array" then error("invalid job page") else .jobs end)
    | add
    | map(if (.id | type) != "number" or (.name | type) != "string"
             or (.status | type) != "string" then error("invalid job") else . end)
    | map(select(.name == $name))
    | if length != 1 then error("producer not uniquely measured") else .[0] end
    | if .status == "completed" then
        if (.conclusion == "success" or .conclusion == "failure" or .conclusion == "cancelled"
            or .conclusion == "skipped" or .conclusion == "timed_out"
            or .conclusion == "action_required" or .conclusion == "neutral"
            or .conclusion == "stale" or .conclusion == "startup_failure")
        then "completed:" + .conclusion else error("unknown producer conclusion") end
      elif (.status == "queued" or .status == "in_progress" or .status == "waiting"
            or .status == "pending" or .status == "requested") and .conclusion == null
      then .status
      else error("unknown producer status") end
  ' <<< "$pages" 2>/dev/null) || return 1
  # The labels the producer asked for, which name the pool it queues on.
  producer_labels=$(jq -r --arg name "$producer_job" '
    map(.jobs) | add | map(select(.name == $name)) | .[0].labels
    | if type == "array" and length > 0 then join(",") else "unreported" end
  ' <<< "$pages" 2>/dev/null) || producer_labels=unreported
}

# Fail as starved: a distinct status, an annotation the run page shows, and
# the same line in the job summary.
fail_producer_never_started() {
  local message
  message="producer '${producer_job}' never started: still ${state} on runner labels [${producer_labels}] after $1s, beyond the ${max_queue_seconds}s this wait allows; missing artifacts: ${artifacts[*]}"
  echo "$message" >&2
  echo "::error title=Producer never started::${message}"
  if [[ -n ${GITHUB_STEP_SUMMARY:-} ]]; then
    printf '### Producer never started\n\n%s\n' "$message" >> "$GITHUB_STEP_SUMMARY" || true
  fi
  exit "$producer_never_started_status"
}

# Seconds until the limit that refused the read lifts, from that refusal's own
# headers: `retry-after` for a secondary limit, `x-ratelimit-reset` otherwise.
# `GET /rate_limit` is not a substitute. It describes the core bucket, which
# need not be the limit that refused the read, and then reports a fresh
# one-hour window however soon the real limit lifts. Fails when the refusal
# names no reset, so the caller can say so instead of guessing.
seconds_to_reset() {
  local headers retry_after reset now
  # A refused inventory read already carries its headers; re-asking would
  # spend another request against the limit that refused it.
  if [[ -n $refused_headers ]]; then
    headers=$refused_headers
  else
    headers=$(gh api -i "$rate_limited_path" 2> /dev/null | tr -d '\r' || true)
  fi
  retry_after=$(sed -n 's/^[Rr]etry-[Aa]fter:[[:space:]]*\([0-9][0-9]*\)$/\1/p' <<< "$headers" | head -n 1)
  if [[ -n $retry_after ]]; then
    echo "$retry_after"
    return 0
  fi
  reset=$(sed -n 's/^[Xx]-[Rr]ate[Ll]imit-[Rr]eset:[[:space:]]*\([0-9][0-9]*\)$/\1/p' <<< "$headers" | head -n 1)
  if [[ -n $reset ]]; then
    now=$(date +%s)
    echo $(( reset > now ? reset - now + 5 : 0 ))
    return 0
  fi
  return 1
}

# The next poll interval: doubled, capped, and never below the configured one.
next_interval() {
  local doubled=$(( $1 * 2 ))
  (( doubled < interval_seconds )) && doubled=$interval_seconds
  (( doubled > max_interval_seconds )) && doubled=$max_interval_seconds
  echo "$doubled"
}

attempt=0
unmeasured_attempts=0
unmeasured_seconds=0
# Backoff stretches each unmeasured poll, so the poll count alone no longer
# bounds the wait. Unmeasured time keeps the budget the fixed interval gave it.
max_unmeasured_seconds=$((max_unmeasured_attempts * interval_seconds))
rate_limit_seconds=0
queued_since=""
wait_seconds=$interval_seconds
missing=("${artifacts[@]}")
state=""
producer_labels=unreported
polls_since_state_read=0
# True when this poll may reuse a running producer's last observed state
# instead of reading it again. A queued or completed producer is always
# re-read: the first bounds starvation, the second ends the wait.
reuse_running_state() {
  [[ $state == in_progress ]] || return 1
  polls_since_state_read=$((polls_since_state_read + 1))
  if (( polls_since_state_read >= state_every_polls )); then
    polls_since_state_read=0
    return 1
  fi
  return 0
}
while :; do
  attempt=$((attempt + 1))
  api_error=""
  rate_limited=0
  refused_headers=""
  if reuse_running_state || read_producer_state; then
    unmeasured_attempts=0
    unmeasured_seconds=0
    case "$state" in
      queued|waiting|pending|requested)
        # Nothing can have been uploaded yet, so the inventory is not read.
        now=$(date +%s)
        [[ -z $queued_since ]] && queued_since=$now
        if (( now - queued_since > max_queue_seconds )); then
          fail_producer_never_started "$((now - queued_since))"
        fi
        missing=("${artifacts[@]}")
        ;;
      *)
        # A producer that just started may upload soon; poll promptly again.
        if [[ -n $queued_since ]]; then
          queued_since=""
          wait_seconds=$interval_seconds
        fi
        if read_artifacts; then
          echo "run artifacts ready: ${artifacts[*]}"
          exit 0
        fi
        # A completed producer has uploaded everything it will. Read the
        # inventory once more, in case the listing lagged the upload, before
        # declaring an artifact lost.
        if [[ $state == completed:* ]] && [ "$rate_limited" -eq 0 ] && read_artifacts; then
          echo "run artifacts ready: ${artifacts[*]}"
          exit 0
        fi
        if [[ $state == completed:* ]]; then
          if [ "$rate_limited" -eq 0 ]; then
            echo "producer '${producer_job}' completed (${state#completed:}) without readable required artifacts: ${missing[*]}" >&2
            exit 1
          fi
        fi
        ;;
    esac
  else
    state=unmeasured
  fi
  if [ "$rate_limited" -eq 1 ]; then
    if ! reset_seconds=$(seconds_to_reset); then
      echo "the GitHub API is rate limited and the refusal names no reset: ${api_error}; missing artifacts: ${missing[*]}" >&2
      exit 1
    fi
    if [ $((rate_limit_seconds + reset_seconds)) -gt "$max_rate_limit_seconds" ]; then
      echo "the GitHub API is rate limited for another ${reset_seconds}s, beyond the ${max_rate_limit_seconds}s this wait allows (${rate_limit_seconds}s already spent): ${api_error}; missing artifacts: ${missing[*]}" >&2
      exit 1
    fi
    echo "the GitHub API is rate limited; waiting ${reset_seconds}s for the reset: ${api_error}" >&2
    rate_limit_seconds=$((rate_limit_seconds + reset_seconds))
    sleep "$reset_seconds"
    continue
  fi
  if [[ $state == unmeasured ]]; then
    if [ -n "$api_error" ]; then
      echo "producer state unreadable (poll ${attempt}): ${api_error}" >&2
    fi
    unmeasured_attempts=$((unmeasured_attempts + 1))
    if [ "$unmeasured_attempts" -ge "$max_unmeasured_attempts" ]; then
      echo "producer '${producer_job}' state unmeasured after ${max_unmeasured_attempts} polls; missing artifacts: ${missing[*]}" >&2
      exit 1
    fi
    if (( max_unmeasured_seconds > 0 && unmeasured_seconds + wait_seconds > max_unmeasured_seconds )); then
      echo "producer '${producer_job}' state unmeasured for ${unmeasured_seconds}s after ${unmeasured_attempts} polls; the next wait would pass the ${max_unmeasured_seconds}s budget; missing artifacts: ${missing[*]}" >&2
      exit 1
    fi
    unmeasured_seconds=$((unmeasured_seconds + wait_seconds))
  fi
  if [ "$attempt" -eq 1 ] || [ $((attempt % 6)) -eq 0 ]; then
    echo "waiting for run artifacts (poll ${attempt}, producer ${state}, next in ${wait_seconds}s): ${missing[*]}"
  fi
  sleep "$wait_seconds"
  wait_seconds=$(next_interval "$wait_seconds")
done
