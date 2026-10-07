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
source "$script_root/scripts/lib/release_version.sh"
source "$script_root/scripts/lib/release_consumer_verdict.sh"
release_validate_retirement_request
retired_version=""
if [[ -n "${RETIRE_SOURCE_SHA:-}" ]]; then
  retired_version="$(release_workspace_version < Cargo.toml)"
  release_require_unpublished_retirement "$retired_version" "${RELEASE_PUBLISHED_VERSION:?published predecessor required}"
fi

# One expression reads a workspace version, so the local tree and the branch are
# always compared the same way.
workspace_version() {
  sed -n 's/^version = "\(.*\)"/\1/p' | head -1
}

HARN_RELEASE_ROOT="$root" HARN_BIN="$harn_bin" \
  "$script_root/scripts/prepare_development_version.sh"
actual="$(workspace_version < Cargo.toml)"
if [[ "$actual" != "$expected_version" ]]; then
  echo "error: expected $expected_version after development bump, got $actual" >&2
  exit 1
fi

branch="automation/development-$actual"

# The drift decision that scheduled this job can be minutes old by the time the
# job runs. In the 0.10.129 cutover the
# decision was taken at 05:10Z while main was still on 0.10.128, the bump it was
# about merged at 05:12Z, and this script ran at 05:19Z and opened a second one.
#
# The open-pull-request probe below cannot catch that: a merged bump is
# indistinguishable from a bump nobody ever opened. Re-read the branch itself,
# which is direct evidence of whether the work is still needed, whoever did it.
#
# An unreadable branch refuses rather than proceeding. Opening a second bump on
# a stale base can silently revert whatever landed in between, so "I could not
# check" must not read the same as "there is nothing there".
if ! git fetch --quiet origin main; then
  echo "error: could not fetch origin/main to re-check the development bump; refusing to open one on unproved state" >&2
  exit 1
fi
main_version="$(git show origin/main:Cargo.toml | workspace_version)"
if [[ -z "$main_version" ]]; then
  echo "error: could not read the workspace version on origin/main; refusing to open a development bump on unproved state" >&2
  exit 1
fi
if [[ "$main_version" == "$actual" ]]; then
  echo "Development bump already on main: origin/main reads $main_version; nothing to open"
  if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    {
      echo "version=$actual"
      echo "harn_bin=$harn_bin"
      echo "pr_url="
      echo "skipped=true"
    } >> "$GITHUB_OUTPUT"
  fi
  exit 0
fi
if [[ -n "$retired_version" ]]; then
  [[ "$main_version" == "$retired_version" ]] || {
    echo "error: main changed the identity being retired; refusing a stale retirement" >&2
    exit 1
  }
  # Preparation and credential minting may take minutes. Repeat all external
  # observations at the mutation boundary, using the unmodified main identity.
  release_require_unpublished_retirement "$main_version" "$RELEASE_PUBLISHED_VERSION"
fi
pr_url="$(gh pr list --state open --head "$branch" --json url --jq '.[0].url // empty')"
if [[ -z "$pr_url" ]]; then
  # shellcheck disable=SC2031 # retirement observation changes credentials only in its subshell
  HARN_BRANCH_COMMIT_TOKEN="$GH_TOKEN" \
    HARN_BRANCH_COMMIT_BRANCH="$branch" \
    HARN_BRANCH_COMMIT_BASE_OID="$(git rev-parse HEAD)" \
    HARN_BRANCH_COMMIT_HEADLINE="Start $actual development" \
    "$harn_bin" run --no-sandbox \
      "$script_root/scripts/bump-driver/publish_branch_commit.harn"
  body_file="$(mktemp)"
  trap 'rm -f "$body_file"' EXIT
  if [[ -n "$retired_version" ]]; then
    printf 'Retire failed, never-published v%s and start %s. Carry its notes into the next release; keep %s as the install and comparison base.\n\nRetained producer %s certifies source %s. Promotion %s refused publication after consumer run %s reached failing required job %s. This change does not publish the retired identity.\n' \
      "$retired_version" "$actual" "$RELEASE_PUBLISHED_VERSION" \
      "$RETIRE_PRODUCER_RUN" "$RETIRE_SOURCE_SHA" "$RETIRE_PROMOTION_RUN" \
      "$RETIRE_CONSUMER_RUN" "$RETIRE_FAILED_JOB" > "$body_file"
  else
    printf '%s\n' \
      "Automated post-release cutover to the next patch development identity." \
      "" \
      "Stable release strings now remain exclusive to immutable release commits; mid-cycle builds self-report $actual." \
      > "$body_file"
  fi
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
    echo "skipped=false"
  } >> "$GITHUB_OUTPUT"
fi
