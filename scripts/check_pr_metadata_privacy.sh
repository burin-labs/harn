#!/usr/bin/env bash
# Scan public pull-request title/body text without echoing either field.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
event_path="${1:-${GITHUB_EVENT_PATH:-}}"

if [[ -z "$event_path" || ! -f "$event_path" ]]; then
  echo "error: pull-request event JSON is required" >&2
  exit 2
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "error: jq is required to read pull-request metadata" >&2
  exit 2
fi
if ! jq -e '
  (.pull_request | type) == "object"
  and (.pull_request.title | type) == "string"
  and (.pull_request.title | length) > 0
  and ((.pull_request.body == null) or ((.pull_request.body | type) == "string"))
' "$event_path" >/dev/null; then
  echo "error: event does not contain a typed pull-request title/body" >&2
  echo "pull-request metadata: fields=0 pending=unmeasured" >&2
  exit 2
fi

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/harn-pr-metadata.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT
pending=0
pending_fields=()

scan_field() {
  local field="$1"
  local filter="$2"
  local output="$tmp_dir/$field.txt"
  local status

  set +e
  jq -j "$filter" "$event_path" \
    | "$repo_root/scripts/check_public_product_names.sh" --stdin-label "$field" \
      >"$output" 2>&1
  status=$?
  set -e

  if [[ "$status" -eq 0 ]]; then
    return 0
  fi
  if [[ "$status" -eq 1 ]]; then
    pending=$((pending + 1))
    pending_fields+=("$field")
    return 0
  fi

  cat "$output" >&2
  echo "pull-request metadata: fields=2 pending=unmeasured" >&2
  exit "$status"
}

scan_field pull-request-title '.pull_request.title'
scan_field pull-request-body '.pull_request.body // ""'

printf 'pull-request metadata: fields=2 pending=%d\n' "$pending"
if [[ "$pending" -gt 0 ]]; then
  printf 'pending fields:' >&2
  printf ' %s' "${pending_fields[@]}" >&2
  printf '\n' >&2
  for field in "${pending_fields[@]}"; do
    cat "$tmp_dir/$field.txt" >&2
  done
  exit 1
fi
