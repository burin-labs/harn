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
  *)
    echo "usage: $0 --family-prefix v0-rust-release-<target>- | --all-release-families" >&2
    exit 64
    ;;
esac

pages="$(list_main_cache_pages)"
while IFS= read -r cache_id; do
  [[ -n "$cache_id" ]] || continue
  gh cache delete "$cache_id" --repo "$repository"
done < <(jq -r --arg family_prefix "$family_prefix" "$selector" <<<"$pages")
