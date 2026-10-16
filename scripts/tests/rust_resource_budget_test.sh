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
# macOS prints each listener's full path, as read from an owned Mac mini. An
# exact name match counts these as zero and refuses the host as retired.
ps() { printf '/sbin/launchd\n/Users/ci/actions-runners/w1/bin/Runner.Listener\n/Users/ci/actions-runner/bin/Runner.Listener\n/Users/ci/bin/Not.Runner.Listener\n'; }
[[ $(online_local_runners) == 2 ]]

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

# The decision this file exists for after the hosted kills, asserted on the
# reading the runner actually reports rather than on a tidy one. A "16 GB"
# vendor VM reports 15989 MB, not 16384: some is held back before the kernel
# ever counts it. The first version of this policy was tested at 16384, which
# sat exactly on an integer-division boundary, so it returned two here and one
# on the real box. A round number that lands on a boundary is not a test of
# the boundary.
[[ $(rust_resource_budget "$policy" 4 1 producer 15989) == $'build_jobs=2\ntest_threads=3' ]]

# And it must not be fragile across the band a four-core vendor VM can report,
# since the exact figure varies with kernel and instance. One row proves a
# point; these prove the plateau the point sits on.
for reported in 14500 15000 15989 16384; do
  [[ $(rust_resource_budget "$policy" 4 1 producer "$reported") == $'build_jobs=2\ntest_threads=3' ]]
done

# The negative control: the same box read through the old cores-only
# arithmetic returns the count that died. If a later edit lets the memory term
# stop binding, this is the number that comes back, so assert the two differ
# rather than only asserting the new one.
[[ $(rust_resource_budget "$policy" 4 1 producer 15989) != $'build_jobs=3\ntest_threads=3' ]]

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

# The hosted macOS runner this lane mostly lands on: three cores and a
# reported 7168 MiB. Cores alone allow two compilers, and two or three
# concurrent compilers there swapped 3.8 million pages and took 58 minutes to
# build what one compiler builds in 27 (harn#8599). Memory must bind at one.
[[ $(rust_resource_budget "$policy" 3 1 producer 7168) == $'build_jobs=1\ntest_threads=2' ]]
[[ $(rust_resource_budget "$policy" 3 1 producer 7168) != $'build_jobs=2\ntest_threads=2' ]]

# Exercise the real memory census through the same shape the host uses. The
# Linux rows pin the kernel so they read /proc on any machine that runs this.
uname() { echo Linux; }
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

# macOS has neither /proc nor nproc. Both censuses read sysctl there, in the
# units the hosted runner actually reported.
nproc() { echo 'nproc must not be called on macOS' >&2; return 1; }
uname() { echo Darwin; }
sysctl() {
  case "$2" in
    hw.memsize) echo 7516192768 ;;
    hw.ncpu) echo 3 ;;
    *) return 1 ;;
  esac
}
[[ $(host_memory_mb) == 7168 ]]
[[ $(host_cpu_cores) == 3 ]]
output=$(mktemp "${TMPDIR:-/tmp}/harn-resource-budget-output.XXXXXX")
trap 'rm -f "$diagnostic" "$output"' EXIT
GITHUB_OUTPUT=$output RUNNER_ENVIRONMENT=github-hosted HARN_BUDGET_PROFILE=producer \
  resource_budget_main 2>/dev/null
[[ $(cat "$output") == $'build_jobs=1\ntest_threads=2' ]]

# An unreadable sysctl is unmeasured, never a zero-memory box.
sysctl() { return 1; }
if host_memory_mb 3 2>"$diagnostic"; then
  echo 'failed macOS memory census passed' >&2; exit 1
fi
grep -qx '::error::E2E_RESOURCE_BUDGET_UNMEASURED reason=memory_census_empty cpu_cores=3 online_local_runners=unmeasured memory_mb=unmeasured hw_memsize=absent' "$diagnostic"
unset -f nproc uname sysctl

# The owned host behind run 36987728246: 24 cores, each runner under a
# 400% CPU quota (so nproc reads 4), 62906 MB, six listeners, two of them
# running jobs. The old reading divided the quota by all six listeners and
# answered one compiler; the allotment is already this job's share, and the
# memory divides by the two running jobs.
[[ $(rust_resource_budget "$policy" 4 6 producer 62906 2>/dev/null) == $'build_jobs=1\ntest_threads=1' ]]
[[ $(rust_resource_budget "$policy" 4 2 producer 62906 true 2>/dev/null) == $'build_jobs=4\ntest_threads=4' ]]
# Saturated: all six running, memory binds the allotment back down.
[[ $(rust_resource_budget "$policy" 4 6 producer 62906 true 2>/dev/null) == $'build_jobs=1\ntest_threads=4' ]]

# Running jobs are counted from Runner.Worker processes, never above the
# listeners, and an unseen worker falls back to the listener count by name.
ps() { printf 'Runner.Listener\nRunner.Listener\nRunner.Worker\nsshd\n'; }
[[ $(running_local_jobs 2) == 1 ]]
ps() { printf 'Runner.Worker\nRunner.Worker\nRunner.Worker\n'; }
[[ $(running_local_jobs 2) == 2 ]]
ps() { printf 'Runner.Listener\nsshd\n'; }
[[ $(running_local_jobs 6 2>"$diagnostic") == 6 ]]
grep -q 'RUST_RESOURCE_BUDGET_WORKERS_UNSEEN listeners=6' "$diagnostic"
unset -f ps

# The actual kernel-reader path sees the installed w6 capacity, independently
# of nproc's version or the host's unrelated worker. No file absence is max.
fixture=$(mktemp -d "${TMPDIR:-/tmp}/harn-cgroup-budget.XXXXXX")
trap 'rm -f "$diagnostic" "$output"; rm -rf "$fixture"' EXIT
proc="$fixture/proc"
cg="$fixture/cgroup"
mkdir -p "$proc/self" "$proc/101" "$proc/102" "$cg/system.slice/w6" "$cg/system.slice/w1"
printf 'cpuset cpu io memory pids\n' > "$cg/cgroup.controllers"
printf '0::/system.slice/w6\n' > "$proc/self/cgroup"
printf '0::/system.slice/w6\n' > "$proc/101/cgroup"
printf '0::/system.slice/w1\n' > "$proc/102/cgroup"
printf '37 27 0:31 / %s rw - cgroup2 cgroup2 rw\n' "$cg" > "$proc/self/mountinfo"
printf 'max 100000\n' > "$cg/system.slice/cpu.max"
printf 'max\n' > "$cg/system.slice/memory.max"
printf 'max\n' > "$cg/system.slice/memory.high"
printf '500000 100000\n' > "$cg/system.slice/w6/cpu.max"
printf '21474836480\n' > "$cg/system.slice/w6/memory.max"
printf '19327352832\n' > "$cg/system.slice/w6/memory.high"
ps() { printf '101 Runner.Worker\n102 Runner.Worker\n103 sshd\n'; }
linux_resource_limits "$proc" self 32 128278 2 self-hosted 2>"$diagnostic"
[[ "$effective_cores:$effective_cpu_jobs:$effective_cpu_allotment" == 5:1:true ]]
[[ "$effective_memory_mb:$effective_memory_jobs:$effective_memory_allotment" == 18432:1:true ]]
[[ $(rust_resource_budget "$policy" "$effective_cores" 2 producer "$effective_memory_mb" \
  "$effective_cpu_allotment" "$effective_memory_allotment" "$effective_cpu_jobs" "$effective_memory_jobs" 2>/dev/null) == $'build_jobs=2\ntest_threads=5' ]]
grep -q 'measured=1 domain=/system.slice/w6 cpu_max=500000 cpu_period=100000 memory_mb=18432 sharing_jobs=1' "$diagnostic"
# Hard limit remains visible when there is no throttling threshold.
printf 'max\n' > "$cg/system.slice/w6/memory.high"
linux_resource_limits "$proc" self 32 128278 2 self-hosted 2>/dev/null
[[ "$effective_memory_mb:$effective_memory_jobs" == 20480:1 ]]
# Affinity limits execution too, but is not divided again by unrelated jobs.
linux_resource_limits "$proc" self 32 128278 2 self-hosted 4 2>/dev/null
[[ "$effective_cores:$effective_cpu_jobs" == 4:1 ]]

# A shared parent can bind more tightly per job than a narrower child. Count
# both real workers in that domain instead of treating every quota as private.
printf '600000 100000\n' > "$cg/system.slice/cpu.max"
printf '15032385536\n' > "$cg/system.slice/memory.max"
linux_resource_limits "$proc" self 32 128278 2 self-hosted 2>/dev/null
[[ "$effective_cores:$effective_cpu_jobs" == 6:2 ]]
[[ "$effective_memory_mb:$effective_memory_jobs" == 14336:2 ]]
[[ $(rust_resource_budget "$policy" "$effective_cores" 2 producer "$effective_memory_mb" \
  "$effective_cpu_allotment" "$effective_memory_allotment" "$effective_cpu_jobs" "$effective_memory_jobs" 2>/dev/null) == $'build_jobs=1\ntest_threads=3' ]]

# Fully measured unlimited hierarchy retains the host's shared capacity.
printf 'max 100000\n' > "$cg/system.slice/cpu.max"
printf 'max\n' > "$cg/system.slice/memory.max"
printf 'max 100000\n' > "$cg/system.slice/w6/cpu.max"
printf 'max\n' > "$cg/system.slice/w6/memory.max"
linux_resource_limits "$proc" self 32 128278 2 self-hosted 2>/dev/null
[[ "$effective_cores:$effective_cpu_jobs:$effective_cpu_allotment" == 32:2:false ]]
[[ "$effective_memory_mb:$effective_memory_jobs:$effective_memory_allotment" == 128278:2:false ]]

for field in cpu.max memory.max memory.high; do
  mv "$cg/system.slice/w6/$field" "$fixture/saved"
  if linux_resource_limits "$proc" self 32 128278 2 self-hosted 2>"$diagnostic"; then
    echo "missing cgroup $field passed" >&2; exit 1
  fi
  grep -q 'reason=cgroup_.*_unmeasured' "$diagnostic"
  mv "$fixture/saved" "$cg/system.slice/w6/$field"
  cp "$cg/system.slice/w6/$field" "$fixture/saved"
  printf 'malformed\n' > "$cg/system.slice/w6/$field"
  if linux_resource_limits "$proc" self 32 128278 2 self-hosted 2>"$diagnostic"; then
    echo "malformed cgroup $field passed" >&2; exit 1
  fi
  grep -q 'reason=cgroup_.*_malformed' "$diagnostic"
  mv "$fixture/saved" "$cg/system.slice/w6/$field"
done
mv "$proc/102/cgroup" "$fixture/saved"
if linux_resource_limits "$proc" self 32 128278 2 self-hosted 2>"$diagnostic"; then
  echo 'unreadable worker allocation passed' >&2; exit 1
fi
grep -q 'reason=worker_cgroup_unmeasured' "$diagnostic"
mv "$fixture/saved" "$proc/102/cgroup"
ps() { printf '102 Runner.Worker\n103 sshd\n'; }
if linux_resource_limits "$proc" self 32 128278 2 self-hosted 2>"$diagnostic"; then
  echo 'missing own worker became a private allocation' >&2; exit 1
fi
grep -q 'reason=own_worker_cgroup_unmeasured' "$diagnostic"
ps() { printf '101 Runner.Worker\n102 Runner.Worker\n103 sshd\n'; }
printf 'io pids\n' > "$cg/cgroup.controllers"
if linux_resource_limits "$proc" self 32 128278 2 self-hosted 2>"$diagnostic"; then
  echo 'unknown root controllers became unlimited' >&2; exit 1
fi
grep -q 'reason=cgroup_controllers_unmeasured' "$diagnostic"
printf 'cpuset cpu io memory pids\n' > "$cg/cgroup.controllers"
printf '2:cpu:/legacy\n' > "$proc/self/cgroup"
if linux_resource_limits "$proc" self 32 128278 2 self-hosted 2>"$diagnostic"; then
  echo 'unsupported hierarchy became host capacity' >&2; exit 1
fi
grep -q 'reason=cgroup_path_unmeasured' "$diagnostic"
unset -f ps

echo 'Rust resource budget: CPU, memory, profile ceilings, hosted Linux and macOS decisions, macOS census, removal, retired-pool, empty and failed census, CPU allotment and running-job controls passed'
