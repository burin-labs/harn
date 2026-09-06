#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
source "$root/scripts/ci/e2e_resource_budget.sh"
policy="$root/scripts/ci/rust-resource-policy.json"
diagnostic=$(mktemp "${TMPDIR:-/tmp}/harn-e2e-resource-budget.XXXXXX")
trap 'rm -f "$diagnostic"' EXIT
[[ $(e2e_resource_budget "$policy" 24 6) == $'build_jobs=2\ntest_threads=3' ]]
[[ $(e2e_resource_budget "$policy" 8 6) == $'build_jobs=1\ntest_threads=1' ]]
[[ $(e2e_resource_budget "$policy" 2 1) == $'build_jobs=1\ntest_threads=1' ]]
for measurement in '24 0' '0 6' '24 unknown'; do
  read -r cores runners <<< "$measurement"
  if e2e_resource_budget "$policy" "$cores" "$runners" 2>"$diagnostic"; then
    echo 'invalid resource census passed' >&2; exit 1
  fi
  grep -q 'E2E_RESOURCE_BUDGET_UNMEASURED' "$diagnostic"
done
# Exercise the actual process census, including removal and an empty read.
ps() { printf '  101\n  202\n'; }
[[ $(e2e_online_local_runners) == 2 ]]
ps() { printf '  101\n'; }
[[ $(e2e_online_local_runners) == 1 ]]
ps() { return 0; }
if e2e_online_local_runners 24 2>"$diagnostic"; then
  echo 'empty listener census passed' >&2; exit 1
fi
grep -qx '::error::E2E_RESOURCE_BUDGET_UNMEASURED reason=listener_census_empty cpu_cores=24 online_local_runners=0 listener_processes=0 census_status=0' "$diagnostic"
ps() { return 1; }
if e2e_online_local_runners 24 2>"$diagnostic"; then
  echo 'failed listener census passed' >&2; exit 1
fi
grep -qx '::error::E2E_RESOURCE_BUDGET_UNMEASURED reason=listener_census_failed cpu_cores=24 online_local_runners=unmeasured listener_processes=unmeasured census_status=1' "$diagnostic"
echo 'E2E resource budget: CPU, memory, removal, empty and failed census controls passed'
