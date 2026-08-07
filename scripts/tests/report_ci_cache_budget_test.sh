#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

mkdir -p "$tmp/bin"
cat >"$tmp/bin/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
args="$*"
printf '%s\n' "$args" >>"$MOCK_GH_LOG"
if [[ "$args" == *'/actions/cache/usage'* ]]; then
  if [[ "${MOCK_API_ERROR:-}" == "usage" ]]; then
    echo "mock API authorization failure" >&2
    exit 1
  fi
  printf '{"full_name":"burin-labs/harn","active_caches_size_in_bytes":%s,"active_caches_count":2}\n' "${MOCK_USAGE_BYTES:-3000}"
elif [[ "$args" == *'/actions/caches?ref=refs/heads/main&per_page=100'* ]]; then
  if [[ "${MOCK_API_ERROR:-}" == "retention" ]]; then
    echo "mock retention API authorization failure" >&2
    exit 1
  fi
  if [[ "${MOCK_DUPLICATE_RELEASE:-0}" == "1" ]]; then
    printf '[{"total_count":2,"actions_caches":[{"id":1,"ref":"refs/heads/main","key":"v0-rust-release-x86_64-unknown-linux-gnu-Linux-x64-11111111-aaaaaaaa","size_in_bytes":2000,"created_at":"2026-01-01T00:00:00Z"},{"id":3,"ref":"refs/heads/main","key":"v0-rust-release-x86_64-unknown-linux-gnu-Linux-x64-22222222-bbbbbbbb","size_in_bytes":2100,"created_at":"2026-01-02T00:00:00Z"}]}]\n'
  else
    printf '%s\n' '[{"actions_caches":[{"id":10,"ref":"refs/heads/main","key":"v0-rust-release-x86_64-unknown-linux-gnu-Linux-x64-11111111-aaaaaaaa","created_at":"2026-01-01T00:00:00Z"},{"id":20,"ref":"refs/heads/main","key":"v0-rust-release-aarch64-apple-darwin-Darwin-arm64-11111111-aaaaaaaa","created_at":"2026-01-01T00:00:00Z"},{"id":30,"ref":"refs/heads/main","key":"v0-rust-workspace-tests-Linux-x64-11111111-aaaaaaaa","created_at":"2026-01-01T00:00:00Z"}]},{"actions_caches":[{"id":11,"ref":"refs/heads/main","key":"v0-rust-release-x86_64-unknown-linux-gnu-Linux-x64-22222222-bbbbbbbb","created_at":"2026-01-02T00:00:00Z"},{"id":12,"ref":"refs/heads/main","key":"v0-rust-release-x86_64-unknown-linux-gnu-Linux-x64-33333333-cccccccc","created_at":"2026-01-02T00:00:00Z"},{"id":21,"ref":"refs/heads/main","key":"v0-rust-release-aarch64-apple-darwin-Darwin-arm64-22222222-bbbbbbbb","created_at":"2026-01-02T00:00:00Z"},{"id":13,"ref":"refs/pull/9/merge","key":"v0-rust-release-x86_64-unknown-linux-gnu-Linux-x64-44444444-dddddddd","created_at":"2026-01-03T00:00:00Z"},{"id":14,"ref":"refs/heads/main","key":"v0-rust-release-x86_64-unknown-linux-gnu-manual-backup","created_at":"2026-01-04T00:00:00Z"}]}]'
  fi
elif [[ "$args" == *'/actions/caches?per_page=100'* ]]; then
  if [[ "${MOCK_BUDGET_INVENTORY:-0}" == "1" ]]; then
    if grep -q '^cache delete 103 --repo burin-labs/harn$' "$MOCK_GH_LOG"; then
      printf '%s\n' '[{"actions_caches":[{"id":101,"ref":"refs/heads/main","key":"v0-rust-release-x86_64-unknown-linux-gnu-current","size_in_bytes":419430400},{"id":102,"ref":"refs/heads/main","key":"v0-rust-workspace-windows-current","size_in_bytes":524288000},{"id":104,"ref":"refs/heads/main","key":"node-tooling-current","size_in_bytes":104857600}]}]'
    else
      printf '%s\n' '[{"actions_caches":[{"id":101,"ref":"refs/heads/main","key":"v0-rust-release-x86_64-unknown-linux-gnu-current","size_in_bytes":419430400},{"id":102,"ref":"refs/heads/main","key":"v0-rust-workspace-windows-current","size_in_bytes":524288000},{"id":103,"ref":"refs/heads/main","key":"v0-rust-workspace-macos-current","size_in_bytes":314572800},{"id":104,"ref":"refs/heads/main","key":"node-tooling-current","size_in_bytes":104857600}]}]'
    fi
  elif [[ "${MOCK_DUPLICATE_RELEASE:-0}" == "1" ]]; then
    printf '[{"total_count":3,"actions_caches":[{"id":1,"ref":"refs/heads/main","key":"v0-rust-release-x86_64-unknown-linux-gnu-Linux-x64-oldhash-oldlock","size_in_bytes":2000,"created_at":"2026-01-01T00:00:00Z"},{"id":3,"ref":"refs/heads/main","key":"v0-rust-release-x86_64-unknown-linux-gnu-Linux-x64-newhash-newlock","size_in_bytes":2100,"created_at":"2026-01-02T00:00:00Z"},{"id":2,"ref":"refs/pull/9/merge","key":"sccache/a/b/c","size_in_bytes":1000,"created_at":"2026-01-01T00:00:00Z"}]}]\n'
  else
    printf '[{"total_count":2,"actions_caches":[{"id":1,"ref":"refs/heads/main","key":"v0-rust-release-linux","size_in_bytes":%s},{"id":2,"ref":"refs/pull/9/merge","key":"sccache/a/b/c","size_in_bytes":1000}]}]\n' "${MOCK_LISTED_RELEASE_BYTES:-2000}"
  fi
elif [[ "$args" == cache\ delete\ *\ --repo\ burin-labs/harn ]]; then
  exit 0
else
  echo "unexpected gh arguments: $args" >&2
  exit 64
fi
MOCK
chmod +x "$tmp/bin/gh"

PATH="$tmp/bin:$PATH" MOCK_GH_LOG="$tmp/gh.log" GITHUB_REPOSITORY=burin-labs/harn \
  GITHUB_STEP_SUMMARY="$tmp/summary.md" \
  "$repo_root/scripts/report_ci_cache_budget.sh" >"$tmp/report.json"
jq -e '
  .schema_version == "harn.ci_cache_budget.v3" and
  .configured_limit_bytes == 10737418240 and
  .active_bytes == 3000 and
  .listed_bytes == 3000 and
  .within_budget == true and
  (.by_class | map(.class) | sort) == ["release", "sccache"]
' "$tmp/report.json" >/dev/null
grep -q 'GitHub Actions cache budget' "$tmp/summary.md"
cat >"$tmp/expected-gh.log" <<'EXPECTED'
api repos/burin-labs/harn/actions/cache/usage
api --paginate repos/burin-labs/harn/actions/caches?per_page=100 --slurp
EXPECTED
diff -u "$tmp/expected-gh.log" "$tmp/gh.log"

PATH="$tmp/bin:$PATH" MOCK_GH_LOG="$tmp/family-prune-gh.log" \
  GITHUB_REPOSITORY=burin-labs/harn \
  "$repo_root/scripts/prune_ci_cache_generations.sh" \
  --family-prefix v0-rust-release-x86_64-unknown-linux-gnu-
cat >"$tmp/expected-family-prune-gh.log" <<'EXPECTED'
api --paginate repos/burin-labs/harn/actions/caches?ref=refs/heads/main&per_page=100 --slurp
cache delete 11 --repo burin-labs/harn
cache delete 10 --repo burin-labs/harn
EXPECTED
diff -u "$tmp/expected-family-prune-gh.log" "$tmp/family-prune-gh.log"

PATH="$tmp/bin:$PATH" MOCK_GH_LOG="$tmp/all-prune-gh.log" \
  GITHUB_REPOSITORY=burin-labs/harn \
  "$repo_root/scripts/prune_ci_cache_generations.sh" --all-release-families
cat >"$tmp/expected-all-prune-gh.log" <<'EXPECTED'
api --paginate repos/burin-labs/harn/actions/caches?ref=refs/heads/main&per_page=100 --slurp
cache delete 20 --repo burin-labs/harn
cache delete 11 --repo burin-labs/harn
cache delete 10 --repo burin-labs/harn
EXPECTED
diff -u "$tmp/expected-all-prune-gh.log" "$tmp/all-prune-gh.log"

if PATH="$tmp/bin:$PATH" MOCK_GH_LOG="$tmp/prune-error-gh.log" MOCK_API_ERROR=retention \
  GITHUB_REPOSITORY=burin-labs/harn \
  "$repo_root/scripts/prune_ci_cache_generations.sh" \
  --family-prefix v0-rust-release-x86_64-unknown-linux-gnu- \
  >"$tmp/prune-error.json" 2>"$tmp/prune-error.err"; then
  echo "expected retention API authorization failure to fail" >&2
  exit 1
fi
grep -q 'mock retention API authorization failure' "$tmp/prune-error.err"

PATH="$tmp/bin:$PATH" MOCK_GH_LOG="$tmp/lagging-gh.log" MOCK_USAGE_BYTES=10737418241 \
  GITHUB_REPOSITORY=burin-labs/harn \
  "$repo_root/scripts/report_ci_cache_budget.sh" >"$tmp/lagging.json" 2>"$tmp/lagging.err"
jq -e '.within_budget == true and .active_bytes > .configured_limit_bytes' \
  "$tmp/lagging.json" >/dev/null
grep -q 'usage telemetry is still above budget' "$tmp/lagging.err"

if PATH="$tmp/bin:$PATH" MOCK_GH_LOG="$tmp/over-gh.log" MOCK_LISTED_RELEASE_BYTES=10737418241 \
  GITHUB_REPOSITORY=burin-labs/harn \
  "$repo_root/scripts/report_ci_cache_budget.sh" >"$tmp/over.json" 2>"$tmp/over.err"; then
  echo "expected over-budget inventory to fail" >&2
  exit 1
fi
grep -q 'cache exceeds the 10737418240-byte policy budget' "$tmp/over.err"

PATH="$tmp/bin:$PATH" MOCK_GH_LOG="$tmp/prune-gh.log" MOCK_DUPLICATE_RELEASE=1 \
  HARN_PRUNE_SUPERSEDED_RELEASE_CACHES=1 GITHUB_REPOSITORY=burin-labs/harn \
  "$repo_root/scripts/report_ci_cache_budget.sh" >"$tmp/prune.json"
grep -Fxq 'cache delete 1 --repo burin-labs/harn' "$tmp/prune-gh.log"

printf '%s\n' '{"storage_limit_bytes":1073741824}' >"$tmp/budget-policy.json"
PATH="$tmp/bin:$PATH" MOCK_GH_LOG="$tmp/budget-prune-gh.log" MOCK_BUDGET_INVENTORY=1 \
  HARN_PRUNE_SUPERSEDED_RELEASE_CACHES=1 HARN_ENFORCE_CACHE_BUDGET=1 \
  HARN_CACHE_POLICY_PATH="$tmp/budget-policy.json" \
  GITHUB_STEP_SUMMARY="$tmp/budget-summary.md" \
  GITHUB_REPOSITORY=burin-labs/harn \
  "$repo_root/scripts/report_ci_cache_budget.sh" >"$tmp/budget-prune.json"
jq -e '
  .within_budget == true and
  .listed_bytes == 1048576000 and
  .budget_enforcement.schema_version == "harn.ci_cache_budget_prune.v1" and
  .budget_enforcement.deficit_bytes == 289406976 and
  .budget_enforcement.selected_bytes == 314572800 and
  (.budget_enforcement.deleted | map(.id)) == [103]
' "$tmp/budget-prune.json" >/dev/null
grep -Fxq 'cache delete 103 --repo burin-labs/harn' "$tmp/budget-prune-gh.log"
grep -q '^#### Budget enforcement$' "$tmp/budget-summary.md"
grep -q 'v0-rust-workspace-macos-current' "$tmp/budget-summary.md"
if grep -Eq '^cache delete (101|102) ' "$tmp/budget-prune-gh.log"; then
  echo "budget enforcement must preserve release and larger high-value caches" >&2
  cat "$tmp/budget-prune-gh.log" >&2
  exit 1
fi

# Linux merge-gate caches are protected: when Windows is the only eligible
# resident that covers the deficit, delete it instead of workspace-tests.
cat >"$tmp/bin/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
args="$*"
printf '%s\n' "$args" >>"$MOCK_GH_LOG"
if [[ "$args" == *'/actions/cache/usage'* ]]; then
  printf '{"full_name":"burin-labs/harn","active_caches_size_in_bytes":2000,"active_caches_count":3}\n'
elif [[ "$args" == *'/actions/caches?per_page=100'* ]]; then
  if grep -q '^cache delete 202 --repo burin-labs/harn$' "$MOCK_GH_LOG"; then
    printf '%s\n' '[{"actions_caches":[{"id":201,"ref":"refs/heads/main","key":"v0-rust-release-x86_64-unknown-linux-gnu-current","size_in_bytes":1610612736},{"id":203,"ref":"refs/heads/main","key":"v0-rust-workspace-tests-Linux-x64-current","size_in_bytes":1610612736}]}]'
  else
    printf '%s\n' '[{"actions_caches":[{"id":201,"ref":"refs/heads/main","key":"v0-rust-release-x86_64-unknown-linux-gnu-current","size_in_bytes":1610612736},{"id":202,"ref":"refs/heads/main","key":"v0-rust-workspace-windows-current","size_in_bytes":3758096384},{"id":203,"ref":"refs/heads/main","key":"v0-rust-workspace-tests-Linux-x64-current","size_in_bytes":1610612736}]}]'
  fi
elif [[ "$args" == cache\ delete\ *\ --repo\ burin-labs/harn ]]; then
  exit 0
else
  echo "unexpected gh arguments: $args" >&2
  exit 64
fi
MOCK
chmod +x "$tmp/bin/gh"
# 5 GiB policy: release 0.4 + windows 3.0 + linux-tests 1.5 = 4.9 GiB.
# Enforcing the budget deletes Windows and keeps the protected Linux graph.
printf '%s\n' '{"storage_limit_bytes":5368709120}' >"$tmp/protect-policy.json"
PATH="$tmp/bin:$PATH" MOCK_GH_LOG="$tmp/protect-gh.log" \
  HARN_ENFORCE_CACHE_BUDGET=1 HARN_CACHE_POLICY_PATH="$tmp/protect-policy.json" \
  GITHUB_REPOSITORY=burin-labs/harn \
  "$repo_root/scripts/report_ci_cache_budget.sh" >"$tmp/protect.json"
jq -e '
  .within_budget == true and
  (.budget_enforcement.deleted | map(.id)) == [202]
' "$tmp/protect.json" >/dev/null
grep -Fxq 'cache delete 202 --repo burin-labs/harn' "$tmp/protect-gh.log"
if grep -Eq '^cache delete (201|203) ' "$tmp/protect-gh.log"; then
  echo "budget enforcement must preserve release and Linux workspace-tests caches" >&2
  cat "$tmp/protect-gh.log" >&2
  exit 1
fi

# Reserve 1.5 GiB headroom under the same 5 GiB ceiling (listed ceiling 3.5 GiB).
PATH="$tmp/bin:$PATH" MOCK_GH_LOG="$tmp/headroom-gh.log" \
  HARN_CACHE_POLICY_PATH="$tmp/protect-policy.json" \
  GITHUB_REPOSITORY=burin-labs/harn \
  "$repo_root/scripts/prune_ci_cache_generations.sh" --ensure-headroom 1610612736 \
  >"$tmp/headroom.json"
jq -e '
  .mode == "ensure_headroom" and
  .listed_ceiling_bytes == 3758096384 and
  (.deleted | map(.id)) == [202]
' "$tmp/headroom.json" >/dev/null

# Restore the baseline mock for the remaining authorization-failure case.
cat >"$tmp/bin/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
args="$*"
printf '%s\n' "$args" >>"$MOCK_GH_LOG"
if [[ "$args" == *'/actions/cache/usage'* ]]; then
  if [[ "${MOCK_API_ERROR:-}" == "usage" ]]; then
    echo "mock API authorization failure" >&2
    exit 1
  fi
  printf '{"full_name":"burin-labs/harn","active_caches_size_in_bytes":%s,"active_caches_count":2}\n' "${MOCK_USAGE_BYTES:-3000}"
elif [[ "$args" == *'/actions/caches?per_page=100'* ]]; then
  printf '[{"total_count":2,"actions_caches":[{"id":1,"ref":"refs/heads/main","key":"v0-rust-release-linux","size_in_bytes":%s},{"id":2,"ref":"refs/pull/9/merge","key":"sccache/a/b/c","size_in_bytes":1000}]}]\n' "${MOCK_LISTED_RELEASE_BYTES:-2000}"
elif [[ "$args" == cache\ delete\ *\ --repo\ burin-labs/harn ]]; then
  exit 0
else
  echo "unexpected gh arguments: $args" >&2
  exit 64
fi
MOCK
chmod +x "$tmp/bin/gh"

if PATH="$tmp/bin:$PATH" MOCK_GH_LOG="$tmp/error-gh.log" MOCK_API_ERROR=usage \
  GITHUB_REPOSITORY=burin-labs/harn \
  "$repo_root/scripts/report_ci_cache_budget.sh" >"$tmp/error.json" 2>"$tmp/error.err"; then
  echo "expected API authorization failure to fail" >&2
  exit 1
fi
grep -q 'mock API authorization failure' "$tmp/error.err"

echo "CI cache budget report tests passed"
