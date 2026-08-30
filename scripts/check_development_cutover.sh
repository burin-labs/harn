#!/usr/bin/env bash
set -euo pipefail

script_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
root="${HARN_DEVELOPMENT_CUTOVER_ROOT:-$script_root}"
repo="${GH_REPO:-burin-labs/harn}"
main_ref="${HARN_DEVELOPMENT_CUTOVER_MAIN_REF:-origin/main}"
pr_rows_file="${HARN_DEVELOPMENT_CUTOVER_PR_ROWS_FILE:-}"

cd "$root"
# shellcheck disable=SC1091 # source path is rooted at this script, not the fixture checkout
source "$script_root/scripts/lib/release_version.sh"

workspace_version="$(
  git show "$main_ref:Cargo.toml" \
    | sed -n 's/^version = "\([^"]*\)"/\1/p' \
    | sed -n '1p'
)"
if [[ -z "$workspace_version" ]]; then
  echo "error: could not read a workspace version from $main_ref:Cargo.toml" >&2
  exit 2
fi

latest_tag="$({
  git tag --merged "$main_ref" --list 'v[0-9]*.[0-9]*.[0-9]*' --sort=-v:refname \
    | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' \
    | sed -n '1p'
} || true)"
if [[ -z "$latest_tag" ]]; then
  echo "error: could not read a stable release tag merged into $main_ref" >&2
  exit 2
fi

expected_version="$(release_next_patch_development "${latest_tag#v}")" || {
  echo "error: latest tag is not a canonical stable version: $latest_tag" >&2
  exit 2
}
expected_branch="automation/development-$expected_version"

if [[ -n "$pr_rows_file" ]]; then
  if [[ ! -f "$pr_rows_file" ]]; then
    echo "error: fixture PR rows file does not exist: $pr_rows_file" >&2
    exit 2
  fi
  pr_rows="$(cat "$pr_rows_file")"
else
  if ! pr_rows="$(
    gh pr list --repo "$repo" --state all --head "$expected_branch" --limit 100 \
      --json state,mergedAt,url,headRefName \
      --jq '.[] | [.headRefName, .state, (.mergedAt // "-"), .url] | @tsv'
  )"; then
    echo "error: could not measure pull requests for $expected_branch" >&2
    exit 2
  fi
fi

matching_count=0
remediation_count=0
open_count=0
merged_count=0
remediations=()
while IFS=$'\t' read -r head state merged_at url; do
  [[ -n "$head" ]] || continue
  if [[ "$head" != "$expected_branch" ]]; then
    echo "error: PR measurement returned unexpected branch $head" >&2
    exit 2
  fi
  if [[ -z "$state" || -z "$url" ]]; then
    echo "error: PR measurement returned an incomplete row for $expected_branch" >&2
    exit 2
  fi
  case "$state" in
    OPEN | CLOSED | MERGED) ;;
    *)
      echo "error: PR measurement returned unknown state $state" >&2
      exit 2
      ;;
  esac
  if [[ "$state" == "MERGED" && "$merged_at" == "-" ]]; then
    echo "error: merged PR measurement omitted mergedAt for $url" >&2
    exit 2
  fi
  matching_count=$((matching_count + 1))
  if [[ "$state" == "OPEN" ]]; then
    open_count=$((open_count + 1))
  fi
  if [[ "$merged_at" != "-" ]]; then
    merged_count=$((merged_count + 1))
  fi
  if [[ "$state" == "OPEN" || "$merged_at" != "-" ]]; then
    remediation_count=$((remediation_count + 1))
    remediations+=("$state:${url:-<no-url>}")
  fi
done <<< "$pr_rows"

echo "main_ref=$main_ref"
echo "main_version=$workspace_version"
echo "latest_tag=$latest_tag"
echo "expected_development_version=$expected_version"
echo "expected_branch=$expected_branch"
echo "matching_pr_count=$matching_count"
echo "remediation_pr_count=$remediation_count"
echo "open_pr_count=$open_count"
echo "merged_pr_count=$merged_count"
if (( remediation_count > 0 )); then
  printf 'remediation_prs=%s\n' "${remediations[*]}"
else
  echo "remediation_prs=<none>"
fi

description="current: main $workspace_version, expected $expected_version, open PRs $open_count"
if [[ "$workspace_version" != "$expected_version" && "$open_count" -eq 0 ]]; then
  description="owed: main $workspace_version, expected $expected_version, no open $expected_branch PR"
  if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    echo "description=$description" >> "$GITHUB_OUTPUT"
  fi
  echo "error: development cutover is owed; $description" >&2
  exit 1
fi

if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
  echo "description=$description" >> "$GITHUB_OUTPUT"
fi
echo "development cutover monitor: $description"
