#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
source "$root/scripts/ci/rust_resource_budget.sh"
policy="$root/scripts/ci/rust-resource-policy.json"
diagnostic=$(mktemp "${TMPDIR:-/tmp}/harn-e2e-resource-budget.XXXXXX")
trap 'rm -f "$diagnostic"' EXIT
# Memory is divided by the listeners sharing the host, exactly as cores are,
# so these owned-box rows state the box's memory rather than leaving it
# implicit. At this size the memory term does not bind and the answers are
# the ones this file already asserted.
[[ $(rust_resource_budget "$policy" 24 6 e2e 131072) == $'build_jobs=2\ntest_threads=3' ]]
# The same shared host with a quarter of the memory does drop, and it drops
# because of the listeners it shares with, not its cores.
[[ $(rust_resource_budget "$policy" 24 6 e2e 32768) == $'build_jobs=1\ntest_threads=3' ]]
[[ $(rust_resource_budget "$policy" 8 6 e2e 131072) == $'build_jobs=1\ntest_threads=1' ]]
[[ $(rust_resource_budget "$policy" 2 1 e2e 8192) == $'build_jobs=1\ntest_threads=1' ]]
for measurement in '24 0' '0 6' '24 unknown'; do
  read -r cores runners <<< "$measurement"
  if rust_resource_budget "$policy" "$cores" "$runners" e2e 65536 2>"$diagnostic"; then
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


# The producer ceiling is measured: eight concurrent compilers cost about
# 2.4 GiB, while the single heavy crate's 8.4 GiB peak is present at every
# setting. A large box stops at the ceiling; a small one, where the old literal
# four oversubscribed the host, gets its own cores minus the reserve.
[[ $(rust_resource_budget "$policy" 24 1 producer 65536) == $'build_jobs=8\ntest_threads=23' ]]
[[ $(rust_resource_budget "$policy" 8 1 producer 65536) == $'build_jobs=7\ntest_threads=7' ]]
[[ $(rust_resource_budget "$policy" 2 1 producer 8192) == $'build_jobs=1\ntest_threads=1' ]]
# The two profiles must not collapse into one another.
[[ $(rust_resource_budget "$policy" 24 1 e2e 65536) == $'build_jobs=2\ntest_threads=23' ]]

# An unknown profile is refused rather than defaulted, because a typo that
# silently selects a ceiling is exactly the failure this file exists to prevent.
if rust_resource_budget "$policy" 24 1 producr 65536 2>/dev/null; then
  echo "unknown profile must be refused" >&2
  exit 1
fi

# The decision this file exists for after the four hosted kills. A four-core
# vendor VM with 16 GB is the box the security archive died on. Cores alone
# allow three compilers there; memory allows two, and the smaller wins.
[[ $(rust_resource_budget "$policy" 4 1 producer 16384) == $'build_jobs=2\ntest_threads=3' ]]

# The negative control for that row: the same box read through the old
# cores-only arithmetic returns the count that died. If a later edit lets the
# memory term stop binding, this is the number that comes back, so assert the
# two differ rather than only asserting the new one.
[[ $(rust_resource_budget "$policy" 4 1 producer 16384) != $'build_jobs=3\ntest_threads=3' ]]

# Memory binds below cores only where memory is scarce. A large owned box is
# unchanged by this policy, which is the claim that keeps the change from
# quietly slowing every other runner.
[[ $(rust_resource_budget "$policy" 24 1 producer 131072) == $'build_jobs=8\ntest_threads=23' ]]

# Halving the box halves the compilers even though the cores did not move.
[[ $(rust_resource_budget "$policy" 24 1 producer 30720) == $'build_jobs=4\ntest_threads=23' ]]

# A tiny box still gets one rather than zero: a floor of zero would stall the
# build outright, which is a worse failure than oversubscribing it.
[[ $(rust_resource_budget "$policy" 4 1 producer 2049) == $'build_jobs=1\ntest_threads=3' ]]

# An unreadable or absent memory reading is refused by name. Before this
# change the function took four arguments and answered from cores alone, so
# the failure mode to guard is a caller that was never updated: it must refuse
# rather than silently return the pre-change answer.
for reading in '' 0 unknown -1; do
  if rust_resource_budget "$policy" 4 1 producer "$reading" 2>"$diagnostic"; then
    echo 'unmeasured memory passed' >&2; exit 1
  fi
  grep -qx "::error::E2E_RESOURCE_BUDGET_UNMEASURED reason=memory_census_not_positive cpu_cores=4 online_local_runners=1 memory_mb=${reading:-unset}" "$diagnostic"
done

# Exercise the real memory census through the same shape the host uses.
cat() { printf 'MemTotal:       16384000 kB\nMemFree:  100 kB\n'; }
[[ $(host_memory_mb) == 16000 ]]

# A meminfo without the field is measurable output that answers nothing, and
# must not read as a zero-memory host.
cat() { printf 'MemFree:  100 kB\n'; }
if host_memory_mb 4 2>"$diagnostic"; then
  echo 'meminfo without MemTotal passed' >&2; exit 1
fi
grep -qx '::error::E2E_RESOURCE_BUDGET_UNMEASURED reason=memory_census_empty cpu_cores=4 online_local_runners=unmeasured memory_mb=unmeasured mem_total_kb=absent' "$diagnostic"

cat() { return 1; }
if host_memory_mb 4 2>"$diagnostic"; then
  echo 'failed memory census passed' >&2; exit 1
fi
grep -qx '::error::E2E_RESOURCE_BUDGET_UNMEASURED reason=memory_census_failed cpu_cores=4 online_local_runners=unmeasured memory_mb=unmeasured' "$diagnostic"
unset -f cat

echo 'Rust resource budget: CPU, memory, profile ceilings, hosted decision, removal, retired-pool, empty and failed census controls passed'
