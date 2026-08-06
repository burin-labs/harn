#!/usr/bin/env bash
# shellcheck disable=SC2016 # jq expands selector variables; the shell must not.
set -euo pipefail

repository="${GITHUB_REPOSITORY:-burin-labs/harn}"
mode="${1:-}"

list_main_cache_pages() {
  gh api --paginate \
    "repos/$repository/actions/caches?ref=refs/heads/main&per_page=100" \
    --slurp
}

list_cache_pages() {
  gh api --paginate \
    "repos/$repository/actions/caches?per_page=100" \
    --slurp
}

case "$mode" in
  --family-prefix)
    family_prefix="${2:-}"
    if [[ "$family_prefix" != v0-rust-release-*- || -n "${3:-}" ]]; then
      echo "usage: $0 --family-prefix v0-rust-release-<target>-" >&2
      exit 64
    fi
    selector='
      [.[].actions_caches[]
        | select(
            .ref == "refs/heads/main"
            and (.key | startswith($family_prefix))
            and (.key | test("-[0-9a-f]{8}-[0-9a-f]{8}$"))
          )
      ]
      | sort_by(.created_at, .id)
      | reverse
      | .[1:][]
      | .id
    '
    ;;
  --all-release-families)
    if [[ -n "${2:-}" ]]; then
      echo "usage: $0 --all-release-families" >&2
      exit 64
    fi
    family_prefix=""
    selector='
      [.[].actions_caches[]
        | select(
            .ref == "refs/heads/main"
            and (.key | startswith("v0-rust-release-"))
            and (.key | test("-[0-9a-f]{8}-[0-9a-f]{8}$"))
          )
        | . + {family: (.key | sub("-[^-]+-[^-]+$"; ""))}
      ]
      | group_by(.family)
      | .[]
      | sort_by(.created_at, .id)
      | reverse
      | .[1:][]
      | .id
    '
    ;;
  --to-budget)
    budget_bytes="${2:-}"
    if [[ ! "$budget_bytes" =~ ^[0-9]+$ ]] || [[ "$budget_bytes" -lt 1073741824 ]] \
      || [[ -n "${3:-}" ]]; then
      echo "usage: $0 --to-budget <bytes-at-least-1GiB>" >&2
      exit 64
    fi

    pages="$(list_cache_pages)"
    plan="$(jq -cer --argjson budget_bytes "$budget_bytes" '
      [.[].actions_caches[]
        | select(
            (.id | type) == "number"
            and (.size_in_bytes | type) == "number"
            and .size_in_bytes >= 0
          )
        | {
            id,
            key,
            ref,
            size_in_bytes,
            created_at: (.created_at // "")
          }
      ] as $caches
      | ($caches | map(.size_in_bytes) | add // 0) as $listed_bytes
      | ([ $caches[] | select(.key | startswith("v0-rust-release-")) ]) as $protected
      | ([ $caches[] | select(.key | startswith("v0-rust-release-") | not) ]) as $eligible
      | ([($listed_bytes - $budget_bytes), 0] | max) as $deficit
      | ($eligible
          | map(select(.size_in_bytes >= $deficit))
          | sort_by(.size_in_bytes, .created_at, .id)
          | .[0] // null) as $single
      | (reduce ($eligible | sort_by(-.size_in_bytes, .created_at, .id)[]) as $cache
          ({selected: [], selected_bytes: 0};
            if .selected_bytes < $deficit then
              .selected += [$cache] | .selected_bytes += $cache.size_in_bytes
            else . end)) as $fallback
      | (if $deficit == 0 then []
          elif $single != null then [$single]
          else $fallback.selected
          end) as $selected
      | {
          schema_version: "harn.ci_cache_budget_prune.v1",
          mode: "to_budget",
          configured_limit_bytes: $budget_bytes,
          listed_bytes_before: $listed_bytes,
          deficit_bytes: $deficit,
          protected_release_bytes: ($protected | map(.size_in_bytes) | add // 0),
          eligible_bytes: ($eligible | map(.size_in_bytes) | add // 0),
          selected_bytes: ($selected | map(.size_in_bytes) | add // 0),
          deleted: $selected
        }
    ' <<<"$pages")"

    deficit_bytes="$(jq -r '.deficit_bytes' <<<"$plan")"
    selected_bytes="$(jq -r '.selected_bytes' <<<"$plan")"
    if [[ "$selected_bytes" -lt "$deficit_bytes" ]]; then
      echo "unable to restore the cache budget without deleting protected release caches" >&2
      exit 1
    fi
    while IFS= read -r cache_id; do
      [[ -n "$cache_id" ]] || continue
      gh cache delete "$cache_id" --repo "$repository"
    done < <(jq -r '.deleted[].id' <<<"$plan")
    printf '%s\n' "$plan"
    exit 0
    ;;
  *)
    echo "usage: $0 --family-prefix v0-rust-release-<target>- | --all-release-families | --to-budget <bytes-at-least-1GiB>" >&2
    exit 64
    ;;
esac

pages="$(list_main_cache_pages)"
while IFS= read -r cache_id; do
  [[ -n "$cache_id" ]] || continue
  gh cache delete "$cache_id" --repo "$repository"
done < <(jq -r --arg family_prefix "$family_prefix" "$selector" <<<"$pages")
