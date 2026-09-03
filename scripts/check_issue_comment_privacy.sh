#!/usr/bin/env bash
# Scan an issue or pull-request comment (and issue/PR bodies) for downstream
# product names and private infrastructure, using the SAME vocabulary as the
# pull-request metadata gate.
#
# Why this exists as its own entry point: `check_pr_metadata_privacy.sh` reads
# a `pull_request` / `merge_group` event and scans titles, bodies and the commit
# range. Comments arrive on `issue_comment` / `issues` events with a different
# JSON shape and no commit range, so they need their own reader — but NOT their
# own word list. Both delegate to `check_public_product_names.sh`, which owns
# the product patterns and the hashed host denylist. A second copy of that
# vocabulary would drift, and the failure mode of drift here is silent
# publication.
#
# This repository is public and comments are exactly where cross-repository work
# names other repositories, so the unguarded surface was the most inviting one.
#
# Like the scanner it wraps, this never echoes matched text. Locations are
# reported by digest so the workflow log does not become a second copy of the
# leak.
#
# Exit codes mirror the scanner: 0 clean, 1 violation, 2 usage/environment.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
event_path="${1:-${GITHUB_EVENT_PATH:-}}"

if [[ -z "$event_path" || ! -f "$event_path" ]]; then
  echo "error: issue_comment or issues event JSON is required" >&2
  exit 2
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "error: jq is required to read comment metadata" >&2
  exit 2
fi

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/harn-comment-privacy.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT
sources="$tmp_dir/sources.tsv"
touch "$sources"

# A source is recorded only when the field is a present, non-empty string.
# Absent fields must not silently count as scanned: this gate's whole failure
# mode is that nothing objects, so an unmeasured source is reported as such
# rather than folded into the clean count.
record_source() {
  local label="$1" filter="$2"
  if jq -e "$filter | strings | select(length > 0)" "$event_path" >/dev/null 2>&1; then
    jq -j "$filter" "$event_path" >"$tmp_dir/$label.txt"
    printf '%s\t%s\n' "$label" "$tmp_dir/$label.txt" >>"$sources"
  fi
}

record_source "comment-body" '.comment.body'
record_source "issue-body" '.issue.body'
record_source "issue-title" '.issue.title'
record_source "pull-request-body" '.pull_request.body'
record_source "pull-request-title" '.pull_request.title'

source_count=0
violations=0
violating_sources=()
while IFS=$'\t' read -r label input; do
  [[ -z "$label" ]] && continue
  source_count=$((source_count + 1))
  output="$tmp_dir/output-$source_count.txt"
  status=0
  "$repo_root/scripts/check_public_product_names.sh" --stdin-label "$label" \
    <"$input" >"$output" 2>&1 || status=$?

  case "$status" in
    0) ;;
    1)
      violations=$((violations + 1))
      violating_sources+=("$label")
      cat "$output" >&2
      ;;
    *)
      cat "$output" >&2
      printf 'comment privacy: sources=%d result=unmeasured\n' "$source_count" >&2
      exit "$status"
      ;;
  esac
done <"$sources"

# Zero sources is not a pass. An event shape this script cannot read would
# otherwise scan nothing and report success, which is the exact shape of the
# defect this gate was filed for.
if [[ "$source_count" -eq 0 ]]; then
  echo "error: no readable comment or body field in this event; nothing was scanned" >&2
  printf 'comment privacy: sources=0 result=unmeasured\n' >&2
  exit 2
fi

if [[ "$violations" -gt 0 ]]; then
  printf 'comment privacy: sources=%d violations=%d in %s\n' \
    "$source_count" "$violations" "${violating_sources[*]}" >&2
  exit 1
fi

printf 'comment privacy: sources=%d violations=0\n' "$source_count"
