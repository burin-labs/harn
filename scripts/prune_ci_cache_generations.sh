#!/usr/bin/env bash
# shellcheck disable=SC2016 # jq expands selector variables; the shell must not.
set -euo pipefail

repository="${GITHUB_REPOSITORY:-burin-labs/harn}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
policy_path="${HARN_CACHE_POLICY_PATH:-$repo_root/.github/cache-policy.json}"
mode="${1:-}"

# Release artifacts and the Linux merge-gate compile caches share the 10 GiB
# GitHub Actions cache pool. Windows/macOS workspace test graphs are valuable but must
# yield when the pool is full — otherwise the #5003 workspace-tests writer is
# evicted before the next merge_group can restore it. The shared CLI family
# is a merge-gate cache too: every Harn proof lane waits on its build, and when
# it was unprotected each sibling refresh leg's headroom step deleted it as the
# largest eligible entry, so that build compiled cold (#9430).

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

configured_limit_bytes() {
  jq -er '.storage_limit_bytes | select(type == "number" and . >= 1073741824 and floor == .)' \
    "$policy_path"
}

prune_to_listed_ceiling() {
  local ceiling_bytes=$1
  local mode_name=$2
  local pages plan deficit_bytes selected_bytes

  pages="$(list_cache_pages)"
  plan="$(jq -cer \
    --argjson ceiling_bytes "$ceiling_bytes" \
    --arg mode_name "$mode_name" \
    --argjson configured_limit_bytes "$(configured_limit_bytes)" \
    '
      def linux_merge_gate_key:
        (.key | startswith("v0-rust-workspace-tests"))
        or (.key | startswith("v0-rust-package-audit"))
        or (.key | startswith("v0-rust-harn-ci-cli"));
      def protected_key:
        linux_merge_gate_key
        or (
          $mode_name != "ensure_headroom"
          and (.key | startswith("v0-rust-release-"))
        );
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
      | ([ $caches[] | select(protected_key) ]) as $protected
      | ([ $caches[] | select(protected_key | not) ]) as $eligible
      | ([($listed_bytes - $ceiling_bytes), 0] | max) as $deficit
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
          mode: $mode_name,
          configured_limit_bytes: $configured_limit_bytes,
          listed_ceiling_bytes: $ceiling_bytes,
          listed_bytes_before: $listed_bytes,
          deficit_bytes: $deficit,
          protected_bytes: ($protected | map(.size_in_bytes) | add // 0),
          protected_release_bytes: (
            [$protected[] | select(.key | startswith("v0-rust-release-"))]
            | map(.size_in_bytes)
            | add // 0
          ),
          eligible_bytes: ($eligible | map(.size_in_bytes) | add // 0),
          selected_bytes: ($selected | map(.size_in_bytes) | add // 0),
          deleted: $selected
        }
    ' <<<"$pages")"

  deficit_bytes="$(jq -r '.deficit_bytes' <<<"$plan")"
  selected_bytes="$(jq -r '.selected_bytes' <<<"$plan")"
  if [[ "$selected_bytes" -lt "$deficit_bytes" ]]; then
    echo "unable to restore the cache budget without deleting protected CI caches" >&2
    exit 1
  fi
  while IFS= read -r cache_id; do
    [[ -n "$cache_id" ]] || continue
    gh cache delete "$cache_id" --repo "$repository"
  done < <(jq -r '.deleted[].id' <<<"$plan")
  printf '%s\n' "$plan"
}

# A family keyed by the full commit SHA keeps only its newest main generation.
per_commit_family_selector='
  [.[].actions_caches[]
    | select(
        .ref == "refs/heads/main"
        and (.key | startswith($family_prefix))
        and (.key | test("-[0-9a-f]{40}$"))
      )
  ]
  | sort_by(.created_at, .id)
  | reverse
  | .[1:][]
  | .id
'

usage_modes="--family-prefix v0-rust-release-<target>- | --local-sccache-family-prefix <repository>-sccache-local-<cache-key>-<os>-<arch>- | --harn-check-cache-family-prefix harn-check-cache-v<N>-<family>-<os>-<arch>- | --all-release-families | --clear-family-prefix v0-rust-{workspace-tests|package-audit|harn-ci-cli}- | --to-budget <bytes-at-least-1GiB> | --ensure-headroom <positive-bytes>"

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
  --local-sccache-family-prefix)
    family_prefix="${2:-}"
    local_prefix="${repository}-sccache-local-"
    local_identity="${family_prefix#"$local_prefix"}"
    local_identity="${local_identity%-}"
    if [[ "$family_prefix" != "$local_prefix"* \
      || ! "$local_identity" =~ ^[A-Za-z0-9_.-]+-(Linux|Windows|macOS)-(X64|ARM64)$ \
      || -n "${3:-}" ]]; then
      echo "usage: $0 --local-sccache-family-prefix <repository>-sccache-local-<cache-key>-<os>-<arch>-" >&2
      exit 64
    fi
    selector="$per_commit_family_selector"
    ;;
  --harn-check-cache-family-prefix)
    # Keys are harn-check-cache-v<N>-<family>-<os>-<arch>-<commit>, written by
    # .github/actions/harn-check-cache-save.
    family_prefix="${2:-}"
    if [[ ! "$family_prefix" =~ ^harn-check-cache-v[0-9]+-[a-z0-9-]+-(Linux|Windows|macOS)-(X64|ARM64)-$ \
      || -n "${3:-}" ]]; then
      echo "usage: $0 --harn-check-cache-family-prefix harn-check-cache-v<N>-<family>-<os>-<arch>-" >&2
      exit 64
    fi
    selector="$per_commit_family_selector"
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
  --clear-family-prefix)
    family_prefix="${2:-}"
    if [[ "$family_prefix" != "v0-rust-workspace-tests-" \
      && "$family_prefix" != "v0-rust-package-audit-" \
      && "$family_prefix" != "v0-rust-harn-ci-cli-" ]] \
      || [[ -n "${3:-}" ]]; then
      echo "usage: $0 --clear-family-prefix v0-rust-{workspace-tests|package-audit|harn-ci-cli}-" >&2
      exit 64
    fi
    selector='[.[].actions_caches[] | select(.key | startswith($family_prefix)) | .id] | .[]'
    ;;
  --to-budget)
    budget_bytes="${2:-}"
    if [[ ! "$budget_bytes" =~ ^[0-9]+$ ]] || [[ "$budget_bytes" -lt 1073741824 ]] \
      || [[ -n "${3:-}" ]]; then
      echo "usage: $0 --to-budget <bytes-at-least-1GiB>" >&2
      exit 64
    fi
    prune_to_listed_ceiling "$budget_bytes" "to_budget"
    exit 0
    ;;
  --ensure-headroom)
    headroom_bytes="${2:-}"
    if [[ ! "$headroom_bytes" =~ ^[1-9][0-9]*$ ]] || [[ -n "${3:-}" ]]; then
      echo "usage: $0 --ensure-headroom <positive-bytes>" >&2
      exit 64
    fi
    limit_bytes="$(configured_limit_bytes)"
    if (( headroom_bytes >= limit_bytes )); then
      echo "error: headroom ${headroom_bytes} must be smaller than policy limit ${limit_bytes}" >&2
      exit 64
    fi
    ceiling=$((limit_bytes - headroom_bytes))
    prune_to_listed_ceiling "$ceiling" "ensure_headroom"
    exit 0
    ;;
  *)
    echo "usage: $0 $usage_modes" >&2
    exit 64
    ;;
esac

pages="$(list_main_cache_pages)"
while IFS= read -r cache_id; do
  [[ -n "$cache_id" ]] || continue
  gh cache delete "$cache_id" --repo "$repository"
done < <(jq -r --arg family_prefix "$family_prefix" "$selector" <<<"$pages")
