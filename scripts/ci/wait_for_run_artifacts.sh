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
max_attempts="${HARN_ARTIFACT_WAIT_MAX_ATTEMPTS:-66}"
interval_seconds="${HARN_ARTIFACT_WAIT_INTERVAL_SECONDS:-10}"

case "$max_attempts" in
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
for ((attempt = 1; attempt <= max_attempts; attempt++)); do
  names=""
  if names=$(gh api "$api_path" --jq '.artifacts[] | select(.expired == false) | .name' 2>/dev/null); then
    missing=()
    for artifact in "${artifacts[@]}"; do
      if ! grep -Fxq "$artifact" <<< "$names"; then
        missing+=("$artifact")
      fi
    done
    if [ "${#missing[@]}" -eq 0 ]; then
      echo "run artifacts ready: ${artifacts[*]}"
      exit 0
    fi
  else
    missing=("${artifacts[@]}")
  fi

  if [ "$attempt" -eq "$max_attempts" ]; then
    echo "timed out waiting for run artifacts after ${max_attempts} attempts: ${missing[*]}" >&2
    exit 1
  fi
  if [ "$attempt" -eq 1 ] || [ $((attempt % 6)) -eq 0 ]; then
    echo "waiting for run artifacts (attempt ${attempt}/${max_attempts}): ${missing[*]}"
  fi
  sleep "$interval_seconds"
done
