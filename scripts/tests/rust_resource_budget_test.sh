#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
source "$root/scripts/ci/rust_resource_budget.sh"
policy="$root/scripts/ci/rust-resource-policy.json"
diagnostic=$(mktemp "${TMPDIR:-/tmp}/harn-e2e-resource-budget.XXXXXX")
trap 'rm -f "$diagnostic"' EXIT
[[ $(rust_resource_budget "$policy" 24 6) == $'build_jobs=2\ntest_threads=3' ]]
[[ $(rust_resource_budget "$policy" 8 6) == $'build_jobs=1\ntest_threads=1' ]]
[[ $(rust_resource_budget "$policy" 2 1) == $'build_jobs=1\ntest_threads=1' ]]
for measurement in '24 0' '0 6' '24 unknown'; do
  read -r cores runners <<< "$measurement"
  if rust_resource_budget "$policy" "$cores" "$runners" 2>"$diagnostic"; then
    echo 'invalid resource census passed' >&2; exit 1
  fi
  grep -qx "::error::E2E_RESOURCE_BUDGET_UNMEASURED reason=census_not_positive cpu_cores=$cores online_local_runners=$runners" "$diagnostic"
done

# Exercise the actual process census. The stubs emit a process table in the
# shape the real census reads, one command name per line.
ps() { printf 'systemd\nRunner.Listener\nRunner.Listener\nsshd\n'; }
[[ $(online_local_runners) == 2 ]]
ps() { printf 'Runner.Listener\nsshd\n'; }
[[ $(online_local_runners) == 1 ]]

# A retired pool is the case this refusal exists for: the host is alive and
# measurable, and it is running no listeners at all. It must refuse by name
# and carry the observed zero, never fall through as a measured budget and
# never report itself as unmeasured.
ps() { printf 'systemd\nsshd\ncron\n'; }
if online_local_runners 24 2>"$diagnostic"; then
  echo 'retired pool with zero listeners passed' >&2; exit 1
fi
grep -qx '::error::E2E_RESOURCE_BUDGET_UNMEASURED reason=listener_census_empty cpu_cores=24 online_local_runners=0 listener_processes=0 census_status=0' "$diagnostic"

# An empty process table is not a zero-listener host; it is a census that did
# not run, and the two must not share a name or a count.
ps() { return 0; }
if online_local_runners 24 2>"$diagnostic"; then
  echo 'empty process table passed' >&2; exit 1
fi
grep -qx '::error::E2E_RESOURCE_BUDGET_UNMEASURED reason=listener_census_failed cpu_cores=24 online_local_runners=unmeasured listener_processes=unmeasured census_status=0' "$diagnostic"

ps() { return 1; }
if online_local_runners 24 2>"$diagnostic"; then
  echo 'failed listener census passed' >&2; exit 1
fi
grep -qx '::error::E2E_RESOURCE_BUDGET_UNMEASURED reason=listener_census_failed cpu_cores=24 online_local_runners=unmeasured listener_processes=unmeasured census_status=1' "$diagnostic"
unset -f ps

# A missing runner environment is unmeasurable, not a default of one runner.
if (RUNNER_ENVIRONMENT= resource_budget_main) 2>"$diagnostic"; then
  echo 'missing runner environment passed' >&2; exit 1
fi
grep -q 'reason=runner_environment_missing' "$diagnostic"
grep -q 'runner_environment=unset' "$diagnostic"


# The producer ceiling is a memory bound, so measurement may only lower it. A
# large box still stops at the recorded ceiling; a small one, where the old
# literal oversubscribed the host, now gets its own cores minus the reserve.
[[ $(rust_resource_budget "$policy" 24 1 producer) == $'build_jobs=4\ntest_threads=23' ]]
[[ $(rust_resource_budget "$policy" 8 1 producer) == $'build_jobs=4\ntest_threads=7' ]]
[[ $(rust_resource_budget "$policy" 2 1 producer) == $'build_jobs=1\ntest_threads=1' ]]
# The two profiles must not collapse into one another.
[[ $(rust_resource_budget "$policy" 24 1 e2e) == $'build_jobs=2\ntest_threads=23' ]]

# An unknown profile is refused rather than defaulted, because a typo that
# silently selects a ceiling is exactly the failure this file exists to prevent.
if rust_resource_budget "$policy" 24 1 producr 2>/dev/null; then
  echo "unknown profile must be refused" >&2
  exit 1
fi

echo 'Rust resource budget: CPU, profile ceilings, removal, retired-pool, empty and failed census controls passed'
