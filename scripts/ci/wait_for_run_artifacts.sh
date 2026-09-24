#!/usr/bin/env bash
# Wait for immutable v4 artifacts published earlier in this workflow run.
#
# GitHub exposes v4 artifacts through the REST API as soon as their upload step
# completes, but job dependencies are terminal-state barriers. This bounded
# bootstrap wait lets free hosted consumers overlap the producer's test phase
# without weakening the producer's fail-closed result.
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

case "$run_attempt" in
  ''|*[!0-9]*|0) echo "GITHUB_RUN_ATTEMPT must be a positive integer" >&2; exit 2 ;;
esac
case "$max_unmeasured_attempts" in
  ''|*[!0-9]*|0) echo "HARN_ARTIFACT_WAIT_MAX_ATTEMPTS must be a positive integer" >&2; exit 2 ;;
esac
case "$interval_seconds" in
  ''|*[!0-9]*) echo "HARN_ARTIFACT_WAIT_INTERVAL_SECONDS must be a non-negative integer" >&2; exit 2 ;;
esac

artifacts=("$@")
for artifact in "${artifacts[@]}"; do
  case "$artifact" in
    ''|*[!A-Za-z0-9._-]*) echo "invalid artifact name: $artifact" >&2; exit 2 ;;
  esac
done

api_path="/repos/${repository}/actions/runs/${run_id}/artifacts?per_page=100"
jobs_path="/repos/${repository}/actions/runs/${run_id}/attempts/${run_attempt}/jobs?per_page=100"

# An unreadable page is uncertainty, never an empty inventory or completion.
read_artifacts() {
  local pages names
  missing=("${artifacts[@]}")
  if ! pages=$(gh api "$api_path" --paginate --slurp 2>/dev/null); then
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

producer_state() {
  local pages
  pages=$(gh api "$jobs_path" --paginate --slurp 2>/dev/null) || return 1
  jq -er --arg name "$producer_job" '
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
  ' <<< "$pages" 2>/dev/null
}

attempt=0
unmeasured_attempts=0
while :; do
  attempt=$((attempt + 1))
  if read_artifacts; then
    echo "run artifacts ready: ${artifacts[*]}"
    exit 0
  fi
  if state=$(producer_state); then
    unmeasured_attempts=0
    if [[ $state == completed:* ]]; then
      # Upload may have completed between the first inventory and the job read.
      # Re-read after observing terminal state before declaring an artifact lost.
      if read_artifacts; then
        echo "run artifacts ready: ${artifacts[*]}"
        exit 0
      fi
      echo "producer '${producer_job}' completed (${state#completed:}) without readable required artifacts: ${missing[*]}" >&2
      exit 1
    fi
  else
    state=unmeasured
    unmeasured_attempts=$((unmeasured_attempts + 1))
    if [ "$unmeasured_attempts" -ge "$max_unmeasured_attempts" ]; then
      echo "producer '${producer_job}' state unmeasured after ${max_unmeasured_attempts} polls; missing artifacts: ${missing[*]}" >&2
      exit 1
    fi
  fi
  if [ "$attempt" -eq 1 ] || [ $((attempt % 6)) -eq 0 ]; then
    echo "waiting for run artifacts (poll ${attempt}, producer ${state}): ${missing[*]}"
  fi
  sleep "$interval_seconds"
done
