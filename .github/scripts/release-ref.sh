#!/usr/bin/env bash
#
# One owner for "is this a release branch, and which version is it?".
#
# The release harness has produced two head-ref shapes over this repo's life:
#
#   release/v<semver>                     254 PRs, #3953 .. #6436 (last merged 2026-08-09)
#   release-attempt/v<semver>/<40-hex>     97 PRs, first merged 2026-07-19, current
#
# Every consumer must accept BOTH. A matcher pinned to the older shape silently
# stops matching at the cutover and its gate goes quiet without failing — which
# is exactly what happened to four call sites in this repo. `release-smoke.yml`
# and `scripts/check_release_smoke.harn` already accepted both; they were the
# outliers that were right.
#
# Sourced by `.github/workflows/ci.yml` (the `changes` job checks out the repo
# before use) and by `scripts/native_platform_ci_plan.sh`.
#
# GitHub Actions `if:` expressions cannot source this file. The one such call
# site, `.github/workflows/cli-cold-start-budget.yml`, spells the same policy as
# a `startsWith(...) || startsWith(...)` pair and is pinned by
# `scripts/check_ci_cache_policy.harn` so the two cannot drift apart silently.

# Print the semver of a release head ref and return 0, or return 1 for any ref
# that is not a release branch. Deliberately strict about the attempt shape's
# trailing SHA: `release-attempt/v1.2.3` with no commit is not a ref the harness
# produces, and accepting it would hide a malformed branch name.
release_head_ref_version() {
  local ref="${1-}"
  local semver='[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?'

  if [[ "$ref" =~ ^release/v(${semver})$ ]]; then
    printf '%s\n' "${BASH_REMATCH[1]}"
    return 0
  fi
  if [[ "$ref" =~ ^release-attempt/v(${semver})/[0-9a-f]{40}$ ]]; then
    printf '%s\n' "${BASH_REMATCH[1]}"
    return 0
  fi
  return 1
}

# Convenience predicate for callers that only need the yes/no.
is_release_head_ref() {
  release_head_ref_version "${1-}" >/dev/null
}
