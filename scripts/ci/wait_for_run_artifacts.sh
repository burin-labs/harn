#!/usr/bin/env bash
# Wait for immutable v4 artifacts published earlier in this workflow run.
#
# GitHub exposes v4 artifacts through the REST API as soon as their upload step
# completes, but job dependencies are terminal-state barriers. This bounded
# bootstrap wait lets free hosted consumers overlap the producer's test phase
# without weakening the producer's fail-closed result.
#
# The wait spends the workflow token's API budget, which every job in the
# repository shares. So it reads only the producer's state until the producer
# has started (no artifact can exist before then), backs off exponentially,
# gives up by name when the producer never starts, and waits out a rate limit
# for as long as the refusing response itself says, never guessing.
set -euo pipefail

if [ "$#" -eq 0 ]; then
  echo "usage: $0 ARTIFACT [ARTIFACT ...]" >&2
  exit 2
fi

repository="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY must name owner/repo}"
run_id="${GITHUB_RUN_ID:?GITHUB_RUN_ID must identify the current workflow run}"
producer_job="${HARN_EXT_ARTIFACT_PRODUCER_JOB:?HARN_EXT_ARTIFACT_PRODUCER_JOB must name the producing job}"
run_attempt="${GITHUB_RUN_ATTEMPT:?GITHUB_RUN_ATTEMPT must identify the current attempt}"
# Retained for callers using the existing setting: this bounds consecutive
# unreadable producer-state observations, not time spent in a measured queue.
max_unmeasured_attempts="${HARN_ARTIFACT_WAIT_MAX_ATTEMPTS:-66}"
interval_seconds="${HARN_ARTIFACT_WAIT_INTERVAL_SECONDS:-10}"
# A rate-limited API is waited out rather than counted as unmeasured, up to
# this many seconds in total across the whole wait. The default fits inside
# the shortest job that runs this script; a reset further away fails at once,
# naming it, rather than as a job timeout that names nothing.
max_rate_limit_seconds="${HARN_ARTIFACT_WAIT_RATE_LIMIT_MAX_SECONDS:-240}"
# The poll interval doubles after each read that finds nothing new, up to this.
max_interval_seconds="${HARN_ARTIFACT_WAIT_MAX_INTERVAL_SECONDS:-60}"
# A producer still queued after this long is starved, not slow.
max_queue_seconds="${HARN_ARTIFACT_WAIT_MAX_QUEUE_SECONDS:-1800}"

case "$run_attempt" in
  ''|*[!0-9]*|0) echo "GITHUB_RUN_ATTEMPT must be a positive integer" >&2; exit 2 ;;
esac
case "$max_unmeasured_attempts" in
  ''|*[!0-9]*|0) echo "HARN_ARTIFACT_WAIT_MAX_ATTEMPTS must be a positive integer" >&2; exit 2 ;;
esac
case "$interval_seconds" in
  ''|*[!0-9]*) echo "HARN_ARTIFACT_WAIT_INTERVAL_SECONDS must be a non-negative integer" >&2; exit 2 ;;
esac
case "$max_rate_limit_seconds" in
  ''|*[!0-9]*) echo "HARN_ARTIFACT_WAIT_RATE_LIMIT_MAX_SECONDS must be a non-negative integer" >&2; exit 2 ;;
esac
case "$max_interval_seconds" in
  ''|*[!0-9]*) echo "HARN_ARTIFACT_WAIT_MAX_INTERVAL_SECONDS must be a non-negative integer" >&2; exit 2 ;;
esac
case "$max_queue_seconds" in
  ''|*[!0-9]*) echo "HARN_ARTIFACT_WAIT_MAX_QUEUE_SECONDS must be a non-negative integer" >&2; exit 2 ;;
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

# An unreadable page is uncertainty, never an empty inventory or completion.
read_artifacts() {
  local pages names
  missing=("${artifacts[@]}")
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
  ' <<< "$pages" 2>/dev/null)
}

# Seconds until the limit that refused the read lifts, from that refusal's own
# headers: `retry-after` for a secondary limit, `x-ratelimit-reset` otherwise.
# `GET /rate_limit` is not a substitute. It describes the core bucket, which
# need not be the limit that refused the read, and then reports a fresh
# one-hour window however soon the real limit lifts. Fails when the refusal
# names no reset, so the caller can say so instead of guessing.
seconds_to_reset() {
  local headers retry_after reset now
  headers=$(gh api -i "$rate_limited_path" 2> /dev/null | tr -d '\r' || true)
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
rate_limit_seconds=0
queued_since=""
wait_seconds=$interval_seconds
missing=("${artifacts[@]}")
while :; do
  attempt=$((attempt + 1))
  api_error=""
  rate_limited=0
  if read_producer_state; then
    unmeasured_attempts=0
    case "$state" in
      queued|waiting|pending|requested)
        # Nothing can have been uploaded yet, so the inventory is not read.
        now=$(date +%s)
        [[ -z $queued_since ]] && queued_since=$now
        if (( now - queued_since > max_queue_seconds )); then
          echo "producer '${producer_job}' never started: still ${state} after $((now - queued_since))s, beyond the ${max_queue_seconds}s this wait allows; missing artifacts: ${artifacts[*]}" >&2
          exit 1
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
  fi
  if [ "$attempt" -eq 1 ] || [ $((attempt % 6)) -eq 0 ]; then
    echo "waiting for run artifacts (poll ${attempt}, producer ${state}, next in ${wait_seconds}s): ${missing[*]}"
  fi
  sleep "$wait_seconds"
  wait_seconds=$(next_interval "$wait_seconds")
done
