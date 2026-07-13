#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
policy_path="${HARN_CACHE_POLICY_PATH:-$repo_root/.github/cache-policy.json}"
repository="${GITHUB_REPOSITORY:-burin-labs/harn}"

configured_limit="$(jq -er '.storage_limit_bytes | select(type == "number" and . >= 1073741824 and floor == .)' "$policy_path")"
usage_json="$(gh api "repos/$repository/actions/cache/usage")"
pages_json="$(gh api --paginate "repos/$repository/actions/caches?per_page=100" --slurp)"

usage_bytes="$(jq -er '.active_caches_size_in_bytes | select(type == "number" and . >= 0 and floor == .)' <<<"$usage_json")"
usage_count="$(jq -er '.active_caches_count | select(type == "number" and . >= 0 and floor == .)' <<<"$usage_json")"
inventory="$(jq -cer '
  [.[].actions_caches[]] as $c |
  {
    listed_count: ($c | length),
    listed_bytes: (($c | map(.size_in_bytes) | add) // 0),
    by_ref: ($c
      | group_by(.ref)
      | map({ref: .[0].ref, count: length, bytes: (map(.size_in_bytes) | add)})
      | sort_by(-.bytes)),
    by_class: ($c
      | group_by(
          if (.key | startswith("sccache/")) then "sccache"
          elif (.key | startswith("v0-rust-release-")) then "release"
          elif (.key | startswith("v0-rust-")) then "rust"
          else "other"
          end
        )
      | map({
          class: (if (.[0].key | startswith("sccache/")) then "sccache"
            elif (.[0].key | startswith("v0-rust-release-")) then "release"
            elif (.[0].key | startswith("v0-rust-")) then "rust"
            else "other"
            end),
          count: length,
          bytes: (map(.size_in_bytes) | add)
        })
      | sort_by(-.bytes))
  }
' <<<"$pages_json")"
listed_bytes="$(jq -r '.listed_bytes' <<<"$inventory")"

report="$(jq -cn \
  --arg schema_version 'harn.ci_cache_budget.v2' \
  --arg repository "$repository" \
  --argjson configured_limit_bytes "$configured_limit" \
  --argjson active_bytes "$usage_bytes" \
  --argjson active_count "$usage_count" \
  --argjson inventory "$inventory" \
  '{
    schema_version: $schema_version,
    repository: $repository,
    configured_limit_bytes: $configured_limit_bytes,
    active_bytes: $active_bytes,
    active_count: $active_count,
    listed_bytes: $inventory.listed_bytes,
    listed_count: $inventory.listed_count,
    by_ref: $inventory.by_ref,
    by_class: $inventory.by_class,
    within_budget: ($active_bytes <= $configured_limit_bytes and $inventory.listed_bytes <= $configured_limit_bytes)
  }')"

printf '%s\n' "$report"

if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  {
    echo "### GitHub Actions cache budget"
    echo
    echo "| Metric | Value |"
    echo "| --- | ---: |"
    jq -r '"| Policy limit | \(.configured_limit_bytes) bytes |", "| Active usage | \(.active_bytes) bytes / \(.active_count) entries |", "| Listed inventory | \(.listed_bytes) bytes / \(.listed_count) entries |"' <<<"$report"
    echo
    echo "#### By class"
    jq -r '.by_class[] | "- \(.class): \(.bytes) bytes across \(.count) entries"' <<<"$report"
    echo
    echo "#### By ref"
    jq -r '.by_ref[0:10][] | "- `\(.ref)`: \(.bytes) bytes across \(.count) entries"' <<<"$report"
  } >>"$GITHUB_STEP_SUMMARY"
fi

if [[ "$usage_bytes" -gt "$configured_limit" || "$listed_bytes" -gt "$configured_limit" ]]; then
  echo "::warning::GitHub Actions cache exceeds the $configured_limit-byte policy budget (active=$usage_bytes listed=$listed_bytes)" >&2
  exit 1
fi
