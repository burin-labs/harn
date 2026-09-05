#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
source "$root/scripts/ci/e2e_resource_budget.sh"
policy="$root/scripts/ci/rust-resource-policy.json"
[[ $(e2e_resource_budget "$policy" 24 6) == $'build_jobs=2\ntest_threads=3' ]]
[[ $(e2e_resource_budget "$policy" 8 6) == $'build_jobs=1\ntest_threads=1' ]]
[[ $(e2e_resource_budget "$policy" 2 1) == $'build_jobs=1\ntest_threads=1' ]]
for measurement in '24 0' '0 6' '24 unknown'; do
  read -r cores runners <<< "$measurement"
  if e2e_resource_budget "$policy" "$cores" "$runners"; then
    echo 'invalid resource census passed' >&2; exit 1
  fi
done
# Exercise the actual process census, including removal and an empty read.
ps() { printf '  101\n  202\n'; }
[[ $(e2e_online_local_runners) == 2 ]]
ps() { printf '  101\n'; }
[[ $(e2e_online_local_runners) == 1 ]]
ps() { return 0; }
if e2e_online_local_runners; then
  echo 'empty listener census passed' >&2; exit 1
fi
ps() { return 1; }
if e2e_online_local_runners; then
  echo 'failed listener census passed' >&2; exit 1
fi
echo 'E2E resource budget: CPU, memory, removal, empty and failed census controls passed'
