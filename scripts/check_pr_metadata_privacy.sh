#!/usr/bin/env bash
# Scan every public metadata source in a pull-request or merge-group event
# without echoing its contents.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
event_path="${1:-${GITHUB_EVENT_PATH:-}}"

if [[ -z "$event_path" || ! -f "$event_path" ]]; then
  echo "error: pull-request or merge-group event JSON is required" >&2
  exit 2
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "error: jq is required to read public metadata" >&2
  exit 2
fi

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/harn-pr-metadata.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT
sources="$tmp_dir/sources.tsv"
touch "$sources"

event_kind=""
base_sha=""
head_sha=""
if jq -e '
  (.pull_request | type) == "object"
  and (.pull_request.title | type) == "string"
  and (.pull_request.title | length) > 0
  and ((.pull_request.body == null) or ((.pull_request.body | type) == "string"))
  and (.pull_request.base.sha | strings | test("^[0-9a-fA-F]{40,64}$"))
  and (.pull_request.head.sha | strings | test("^[0-9a-fA-F]{40,64}$"))
' "$event_path" >/dev/null; then
  event_kind="pull-request"
  base_sha="$(jq -r '.pull_request.base.sha | ascii_downcase' "$event_path")"
  head_sha="$(jq -r '.pull_request.head.sha | ascii_downcase' "$event_path")"
  jq -j '.pull_request.title' "$event_path" >"$tmp_dir/pull-request-title.txt"
  jq -j '.pull_request.body // ""' "$event_path" >"$tmp_dir/pull-request-body.txt"
  printf 'pull-request-title\t%s\n' "$tmp_dir/pull-request-title.txt" >>"$sources"
  printf 'pull-request-body\t%s\n' "$tmp_dir/pull-request-body.txt" >>"$sources"
elif jq -e '
  (.merge_group | type) == "object"
  and (.merge_group.base_sha | strings | test("^[0-9a-fA-F]{40,64}$"))
  and (.merge_group.head_sha | strings | test("^[0-9a-fA-F]{40,64}$"))
' "$event_path" >/dev/null; then
  event_kind="merge-group"
  base_sha="$(jq -r '.merge_group.base_sha | ascii_downcase' "$event_path")"
  head_sha="$(jq -r '.merge_group.head_sha | ascii_downcase' "$event_path")"
else
  echo "error: event does not contain typed public metadata and a commit range" >&2
  echo "public metadata: sources=0 commits=unmeasured pending=commit-enumeration" >&2
  exit 2
fi

static_sources=0
if [[ "$event_kind" == "pull-request" ]]; then
  static_sources=2
fi

commit_range="$tmp_dir/commits.txt"
if ! git -C "$repo_root" cat-file -e "${base_sha}^{commit}" 2>/dev/null \
  || ! git -C "$repo_root" cat-file -e "${head_sha}^{commit}" 2>/dev/null; then
  echo "error: commit range could not be enumerated" >&2
  printf 'public metadata: sources=%d commits=unmeasured pending=commit-enumeration\n' \
    "$static_sources" >&2
  exit 2
fi
range_base="$(git -C "$repo_root" merge-base "$base_sha" "$head_sha" 2>/dev/null || true)"
if [[ ! "$range_base" =~ ^[0-9a-f]{40,64}$ ]] \
  || { [[ "$event_kind" == "merge-group" ]] && [[ "$range_base" != "$base_sha" ]]; } \
  || ! git -C "$repo_root" rev-list --reverse "${range_base}..${head_sha}" >"$commit_range" 2>/dev/null \
  || [[ ! -s "$commit_range" ]]; then
  echo "error: commit range could not be enumerated" >&2
  printf 'public metadata: sources=%d commits=unmeasured pending=commit-enumeration\n' \
    "$static_sources" >&2
  exit 2
fi

commit_count=0
while IFS= read -r commit; do
  if [[ ! "$commit" =~ ^[0-9a-f]{40,64}$ ]]; then
    echo "error: commit enumeration returned an invalid identifier" >&2
    printf 'public metadata: sources=%d commits=unmeasured pending=commit-enumeration\n' \
      "$static_sources" >&2
    exit 2
  fi
  commit_count=$((commit_count + 1))
  message="$tmp_dir/commit-$commit.txt"
  if ! git -C "$repo_root" show -s --format=%B "$commit" >"$message" 2>/dev/null; then
    echo "error: an enumerated commit message could not be read" >&2
    printf 'public metadata: sources=%d commits=unmeasured pending=commit-enumeration\n' \
      "$static_sources" >&2
    exit 2
  fi
  printf 'commit/%s/message\t%s\n' "$commit" "$message" >>"$sources"
done <"$commit_range"

pending=0
pending_sources=()
source_count=0
while IFS=$'\t' read -r label input; do
  [[ -z "$label" ]] && continue
  source_count=$((source_count + 1))
  output="$tmp_dir/output-$source_count.txt"
  status=0
  "$repo_root/scripts/check_public_product_names.sh" --stdin-label "$label" \
    <"$input" >"$output" 2>&1 || status=$?

  if [[ "$status" -eq 0 ]]; then
    continue
  fi
  if [[ "$status" -eq 1 ]]; then
    pending=$((pending + 1))
    pending_sources+=("$label")
    continue
  fi

  cat "$output" >&2
  printf 'public metadata: sources=%d commits=%d pending=unmeasured\n' \
    "$source_count" "$commit_count" >&2
  exit "$status"
done <"$sources"

printf 'public metadata: sources=%d commits=%d pending=%d\n' \
  "$source_count" "$commit_count" "$pending"
if [[ "$pending" -gt 0 ]]; then
  printf 'pending sources:' >&2
  printf ' %s' "${pending_sources[@]}" >&2
  printf '\n' >&2
  for ((index = 1; index <= source_count; index += 1)); do
    [[ -s "$tmp_dir/output-$index.txt" ]] && cat "$tmp_dir/output-$index.txt" >&2
  done
  exit 1
fi
