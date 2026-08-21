#!/usr/bin/env bash
#
# Guards the one owner of release head-ref recognition, and the one call site
# that cannot use it (a GitHub Actions `if:` expression).
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
# shellcheck source=.github/scripts/release-ref.sh
source "$repo_root/.github/scripts/release-ref.sh"

fail() { echo "release_ref_matcher_test: $*" >&2; exit 1; }

# --- 1. real refs this repo has actually produced ----------------------------
# Both shapes, taken from the real PR population (254 `release/v<semver>`
# PRs #3953..#6436; 97 `release-attempt/v<ver>/<sha>` PRs, current).
assert_version() {
  local ref="$1" want="$2" got
  got=$(release_head_ref_version "$ref") || fail "release ref rejected: $ref"
  [[ "$got" == "$want" ]] || fail "$ref resolved to '$got', expected '$want'"
}
assert_version release/v0.10.68 0.10.68
assert_version release/v0.7.31 0.7.31
assert_version release/v1.2.3-rc.1 1.2.3-rc.1
assert_version release-attempt/v0.10.108/3cfcd38bf22ef4586671d403881e293b39e0de1d 0.10.108
assert_version release-attempt/v0.10.24/74ae76474fdc59439734b73d7e1ecd9186c64dc7 0.10.24
assert_version release-attempt/v1.2.3-rc.1/0123456789abcdef0123456789abcdef01234567 1.2.3-rc.1

# --- 2. NEGATIVE CONTROL -----------------------------------------------------
# A matcher that accepts everything passes section 1 exactly like a correct one,
# so these are what make section 1 mean something. The last three are real head
# refs from this repo that are NOT release branches.
for bad in \
  main \
  feature/x \
  release-attempt/v1.2.3 \
  release-attempt/v1.2.3/not-a-sha \
  release/v1.2 \
  release/vX.Y.Z \
  feature/release/v1.2.3 \
  release-certify/7d635aa822fcaff74caa0962d3099840efb9f57b \
  release-dispatches-fleet-bump \
  release/prepare-v0.7.37
do
  if release_head_ref_version "$bad" >/dev/null 2>&1; then
    fail "non-release ref was accepted: $bad"
  fi
done

# --- 3. the bash call sites must USE the owner, not re-copy the regex --------
for site in .github/workflows/ci.yml scripts/native_platform_ci_plan.sh; do
  grep -q 'release-ref\.sh' "$repo_root/$site" \
    || fail "$site no longer sources .github/scripts/release-ref.sh"
  if grep -qE '\^release/v\[0-9\]' "$repo_root/$site"; then
    fail "$site re-introduced an inline release-ref regex; call the shared matcher instead"
  fi
done

# --- 4. the GHA `if:` that cannot source the owner --------------------------
# It must cover BOTH shapes, and it must equal the constant the cache-policy
# checker pins, so the two projections cannot drift apart silently.
cold_start_if=$(grep -F "github.event_name != 'workflow_dispatch'" \
  "$repo_root/.github/workflows/cli-cold-start-budget.yml" | sed -E 's/^[[:space:]]+//')
[[ -n "$cold_start_if" ]] || fail "could not find the cold-start budget if: expression"
grep -q "startsWith(github.ref_name, 'release/v')" <<<"$cold_start_if" \
  || fail "cold-start if: no longer excludes release/v refs"
grep -q "startsWith(github.ref_name, 'release-attempt/v')" <<<"$cold_start_if" \
  || fail "cold-start if: does not exclude release-attempt/v refs — the exclusion never fires"

policy_if=$(grep -A1 'const CLI_COLD_START_JOB_IF' "$repo_root/scripts/check_ci_cache_policy.harn" \
  | tail -1 | sed -E 's/^[[:space:]]*"//; s/"[[:space:]]*$//')
[[ "$cold_start_if" == "$policy_if" ]] \
  || fail "workflow if: and check_ci_cache_policy.harn disagree:
  workflow: $cold_start_if
  policy  : $policy_if"

echo "release_ref_matcher_test: ok"
