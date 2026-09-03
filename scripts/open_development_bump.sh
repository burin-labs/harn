#!/usr/bin/env bash
set -euo pipefail

script_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
root="${HARN_RELEASE_ROOT:-$script_root}"
cd "$root"

expected_version="${EXPECTED_DEVELOPMENT_VERSION:-}"
harn_bin="${HARN_BIN:-}"
if [[ -z "$expected_version" ]]; then
  echo "error: EXPECTED_DEVELOPMENT_VERSION is required" >&2
  exit 1
fi
if [[ -z "$harn_bin" || ! -x "$harn_bin" ]]; then
  echo "error: HARN_BIN must name the already-built release-source Harn executable" >&2
  exit 1
fi
if [[ -z "${GH_TOKEN:-}" ]]; then
  echo "error: GH_TOKEN is required" >&2
  exit 1
fi

HARN_RELEASE_ROOT="$root" HARN_BIN="$harn_bin" \
  "$script_root/scripts/prepare_development_version.sh"
actual="$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')"
if [[ "$actual" != "$expected_version" ]]; then
  echo "error: expected $expected_version after development bump, got $actual" >&2
  exit 1
fi

branch="automation/development-$actual"
pr_url="$(gh pr list --state open --head "$branch" --json url --jq '.[0].url // empty')"
if [[ -z "$pr_url" ]]; then
  HARN_DEVELOPMENT_BUMP_TOKEN="$GH_TOKEN" \
    HARN_DEVELOPMENT_BUMP_BRANCH="$branch" \
    HARN_DEVELOPMENT_BUMP_BASE_OID="$(git rev-parse HEAD)" \
    HARN_DEVELOPMENT_BUMP_VERSION="$actual" \
    "$harn_bin" run --no-sandbox \
      "$script_root/scripts/bump-driver/publish_development_bump.harn"
  body_file="$(mktemp)"
  trap 'rm -f "$body_file"' EXIT
  printf '%s\n' \
    "Automated post-release cutover to the next patch development identity." \
    "" \
    "Stable release strings now remain exclusive to immutable release commits; mid-cycle builds self-report $actual." \
    > "$body_file"
  pr_url="$(gh pr create \
    --base main \
    --head "$branch" \
    --title "[Release] Start $actual development" \
    --body-file "$body_file")"
  gh pr edit "$pr_url" --add-label no-changelog-needed
  echo "Development bump opened: $pr_url"
else
  echo "Development bump already open: $pr_url"
fi

if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
  {
    echo "version=$actual"
    echo "harn_bin=$harn_bin"
    echo "pr_url=$pr_url"
  } >> "$GITHUB_OUTPUT"
fi
