#!/usr/bin/env bash
# Replay the measured hosted admission failure through the production owner.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
mkdir "$scratch/bin"
cat > "$scratch/bin/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$PROBE_LOG"
if [[ "$*" == 'api --paginate repos/burin-labs/harn/actions/caches?per_page=100 --slurp' ]]; then
  printf '%s\n' '[{"total_count":3,"actions_caches":[
    {"id":1,"ref":"refs/heads/main","key":"v0-rust-workspace-tests-workspace-crates-v3-Linux-x64-2af81dbb-ee9350a1","size_in_bytes":4545588979},
    {"id":2,"ref":"refs/heads/main","key":"v0-rust-harn-ci-cli-workspace-crates-v3-Linux-x64-2af81dbb-74f0840d","size_in_bytes":1956699943},
    {"id":3,"ref":"refs/heads/main","key":"v0-rust-package-audit-workspace-crates-v3-Linux-x64-e8124b01-4b671fb1","size_in_bytes":1808293122}]}]'
else
  echo "unexpected provider call: $*" >&2
  exit 64
fi
MOCK
chmod +x "$scratch/bin/gh"
export PATH="$scratch/bin:$PATH" GITHUB_REPOSITORY=burin-labs/harn
jq '.storage_limit_bytes = 10737418240' "$root/.github/cache-policy.json" > "$scratch/ten.json"
if PROBE_LOG="$scratch/ten.log" HARN_CACHE_POLICY_PATH="$scratch/ten.json" \
  "$root/scripts/prune_ci_cache_generations.sh" --ensure-headroom 8589934592 \
  --replacing v0-rust-workspace-tests- > "$scratch/ten.out" 2> "$scratch/ten.err"; then
  echo 'the measured 10 GiB capacity must refuse rotation' >&2
  exit 1
fi
grep -q 'without deleting protected CI caches' "$scratch/ten.err"
PROBE_LOG="$scratch/twenty.log" HARN_CACHE_POLICY_PATH="$root/.github/cache-policy.json" \
  "$root/scripts/prune_ci_cache_generations.sh" --ensure-headroom 8589934592 \
  --replacing v0-rust-workspace-tests- > "$scratch/twenty.json"
jq -e '.configured_limit_bytes == 21474836480 and .listed_bytes_before == 8310582044 and
  .headroom_bytes == 6818383469 and .deficit_bytes == 0 and .deleted == [] and
  (.protected_generations | map(.id) | sort) == [1,2,3]' "$scratch/twenty.json" > /dev/null
if grep -q '^cache delete ' "$scratch/ten.log" "$scratch/twenty.log"; then
  echo 'the current generations must remain available in both controls' >&2
  exit 1
fi
echo 'Measured cache rotation capacity controls passed'
