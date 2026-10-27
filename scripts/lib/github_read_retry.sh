#!/usr/bin/env bash

# Retry whole read-only API requests, never accepting pages from a failed attempt.
read_api_json() {
  local endpoint="$1" mode="${2:-single}" filter="${3:-}" response status attempt diagnostic
  local flags=()
  case "$mode" in
    single) ;;
    paginated)
      flags=(--paginate)
      [[ -n "$filter" ]] || flags+=(--slurp) ;;
    *) return 1 ;;
  esac
  [[ -z "$filter" ]] || flags+=(--jq "$filter")
  for attempt in 1 2 3; do
    diagnostic=$(mktemp) || return 1
    if response=$(gh api ${flags[@]+"${flags[@]}"} "$endpoint" 2> "$diagnostic"); then
      cat "$diagnostic" >&2
      rm -f "$diagnostic"
      printf '%s\n' "$response"
      return 0
    fi
    cat "$diagnostic" >&2
    status=$(sed -nE -e $'s/\r$//' \
      -e 's/^gh:.* \(HTTP ([0-9]{3})\)$/\1/p' \
      -e 's/^gh: HTTP ([0-9]{3})$/\1/p' "$diagnostic")
    rm -f "$diagnostic"
    if [[ -z "$status" ]]; then
      status=$(jq -er '
        (if type == "array" then .[-1] else . end)
        | select(type == "object") | .status | tonumber
      ' <<< "$response" 2>/dev/null) || status=unreported
    fi
    case "$status" in
      5[0-9][0-9]) ;;
      *) echo "GitHub metadata read refused; no retry: $endpoint" >&2; return 1 ;;
    esac
    echo "GitHub metadata read HTTP $status, attempt $attempt/3: $endpoint" >&2
    (( attempt < 3 )) || return 1
    sleep "$attempt"
  done
}
