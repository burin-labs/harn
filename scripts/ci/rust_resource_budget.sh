#!/usr/bin/env bash
# Bootstrap budget: runs before a source-built CLI is available.
#
# One owner for "how many rustc processes may this job start". A literal in a
# workflow answers that question for the box someone had in mind when they typed
# it, and is wrong on every other box: the same number oversubscribes a small
# hosted runner and leaves an owned machine idle. The count is measured here
# from the host and divided by the listeners actually sharing it, then capped
# per profile.
#
# Both the cores the box has and the memory it has bound this number, and the
# smaller of the two wins. Cores alone said three compilers on a four-core
# vendor VM, which is right about the CPU and wrong about the box: that VM has
# 16 GB, and the security archive's link phase was signalled dead there four
# times in four hours while the same job passes on larger hosts. A budget that
# cannot see memory cannot see that difference.
set -euo pipefail

# Every refusal in this file is named and carries what was actually observed,
# so an unmeasurable host is never mistaken for a measured one.
budget_refuse() {
  local reason=$1 cores=$2 runners=$3
  shift 3
  echo "::error::E2E_RESOURCE_BUDGET_UNMEASURED reason=$reason cpu_cores=$cores online_local_runners=$runners${*:+ $*}" >&2
  return 1
}

rust_resource_budget() {
  local policy=$1 cores=$2 runners=$3 profile=${4:-e2e} memory_mb=$5
  local reserved maximum share reserved_memory per_compiler memory_share
  if [[ ! "$cores" =~ ^[1-9][0-9]*$ || ! "$runners" =~ ^[1-9][0-9]*$ ]]; then
    budget_refuse census_not_positive "$cores" "$runners"
    return 1
  fi
  # Memory is a census like the other two, so an unreadable or absent reading
  # refuses by name here rather than falling through to a cores-only answer
  # that looks measured. The whole point of this change is that the cores-only
  # answer was wrong on a small box, so silently restoring it would restore the
  # defect while reporting a budget.
  if [[ ! "$memory_mb" =~ ^[1-9][0-9]*$ ]]; then
    budget_refuse memory_census_not_positive "$cores" "$runners" \
      "memory_mb=${memory_mb:-unset}"
    return 1
  fi
  if [[ "$profile" != "e2e" && "$profile" != "producer" ]]; then
    budget_refuse profile_unknown "$cores" "$runners" "profile=$profile"
    return 1
  fi
  if ! jq -e '(.schema_version == 3) and
      ([.reserved_host_cores, .reserved_host_memory_mb, .memory_mb_per_compiler,
        .e2e_max_compilers, .producer_max_compilers] |
      all(type == "number" and . >= 1 and . == floor))' "$policy" >/dev/null; then
    budget_refuse policy_invalid "$cores" "$runners" "policy=$policy"
    return 1
  fi
  reserved=$(jq -r .reserved_host_cores "$policy")
  maximum=$(jq -r ".${profile}_max_compilers" "$policy")
  reserved_memory=$(jq -r .reserved_host_memory_mb "$policy")
  per_compiler=$(jq -r .memory_mb_per_compiler "$policy")
  share=$(((cores - reserved) / runners))
  ((share >= 1)) || share=1
  memory_share=$(((memory_mb - reserved_memory) / per_compiler / runners))
  ((memory_share >= 1)) || memory_share=1
  local build=$share
  ((build <= memory_share)) || build=$memory_share
  ((build <= maximum)) || build=$maximum
  echo "RUST_RESOURCE_BUDGET profile=$profile cores=$cores memory_mb=$memory_mb online_local_runners=$runners cpu_share=$share memory_share=$memory_share build_jobs=$build" >&2
  printf 'build_jobs=%s\ntest_threads=%s\n' "$build" "$share"
}

host_memory_mb() {
  # Total rather than available: the number this budget divides has to be a
  # property of the box, not of whatever happened to be cached when the job
  # started. A reading that moves with page cache would hand two runs of the
  # same workflow two different budgets.
  local cores=${1:-unmeasured} meminfo kb
  if ! meminfo=$(cat /proc/meminfo 2>/dev/null); then
    budget_refuse memory_census_failed "$cores" unmeasured "memory_mb=unmeasured"
    return 1
  fi
  kb=$(awk '$1 == "MemTotal:" { print $2; exit }' <<< "$meminfo")
  if [[ ! "$kb" =~ ^[1-9][0-9]*$ ]]; then
    budget_refuse memory_census_empty "$cores" unmeasured \
      "memory_mb=unmeasured mem_total_kb=${kb:-absent}"
    return 1
  fi
  printf '%s\n' "$((kb / 1024))"
}

online_local_runners() {
  # Include idle listeners and runners outside this job's label pool. Their
  # processes share this host, whereas an org-wide pool count spans hosts.
  #
  # Take the whole process table rather than selecting with `ps -C`. A
  # selecting `ps` exits 1 both when the census cannot run and when it runs
  # and matches nothing, so on a host whose pool has been retired the two
  # collapse into one status and a real zero reports itself as unmeasured.
  # The full table is non-empty on any live host, which separates them.
  local cores=${1:-unmeasured} census census_status count
  if census=$(ps -e -o comm=); then
    census_status=0
  else
    census_status=$?
  fi
  if ((census_status != 0)) || [[ -z "${census//[[:space:]]/}" ]]; then
    budget_refuse listener_census_failed "$cores" unmeasured \
      "listener_processes=unmeasured census_status=$census_status"
    return 1
  fi
  count=$(awk '$1 == "Runner.Listener" { count++ } END { print count+0 }' <<< "$census")
  if ((count == 0)); then
    budget_refuse listener_census_empty "$cores" 0 \
      "listener_processes=0 census_status=$census_status"
    return 1
  fi
  printf '%s\n' "$count"
}

resource_budget_main() {
  local runners cores memory_mb policy profile=${HARN_BUDGET_PROFILE:-e2e}
  if ! cores=$(nproc); then
    budget_refuse cpu_census_failed unmeasured unmeasured
    return 1
  fi
  case "${RUNNER_ENVIRONMENT:-}" in
    github-hosted) runners=1 ;;
    self-hosted) runners=$(online_local_runners "$cores") || return 1 ;;
    *)
      budget_refuse runner_environment_missing "$cores" unmeasured \
        "runner_environment=${RUNNER_ENVIRONMENT:-unset}"
      return 1
      ;;
  esac
  # After the environment is resolved, so an unknown tier is still named as an
  # unknown tier rather than as whatever the memory census happened to say.
  memory_mb=$(host_memory_mb "$cores") || return 1
  policy="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/rust-resource-policy.json"
  rust_resource_budget "$policy" "$cores" "$runners" "$profile" "$memory_mb" \
    >> "${GITHUB_OUTPUT:?}"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  resource_budget_main
fi
