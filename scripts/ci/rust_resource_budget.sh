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

resource_policy_valid() {
  jq -e '(.schema_version == 3) and
      ([.reserved_host_cores, .reserved_host_memory_mb, .memory_mb_per_compiler,
        .e2e_max_compilers, .producer_max_compilers] |
      all(type == "number" and . >= 1 and . == floor))' "$1" >/dev/null
}

# $6 says the cores were measured inside this runner's own CPU allotment (a
# cgroup quota narrower than the host). That allotment already is this job's
# share, so dividing it again by the jobs on the host double-counts them: four
# allotted cores across six listeners read as one compiler on an owned host
# whose other five runners were idle, and the shared CLI hit its 31-minute
# step timeout (run 36987728246).
rust_resource_budget() {
  local policy=$1 cores=$2 runners=$3 profile=${4:-e2e} memory_mb=$5
  local cpu_allotment=${6:-false}
  local memory_allotment=${7:-false} cpu_jobs=${8:-$runners} memory_jobs=${9:-$runners}
  local reserved maximum share reserved_memory per_compiler memory_share
  if [[ ! "$cores" =~ ^[1-9][0-9]*$ || ! "$runners" =~ ^[1-9][0-9]*$ ]]; then
    budget_refuse census_not_positive "$cores" "$runners"
    return 1
  fi
  if [[ ! "$cpu_jobs" =~ ^[1-9][0-9]*$ || ! "$memory_jobs" =~ ^[1-9][0-9]*$ ||
        "$cpu_allotment" != true && "$cpu_allotment" != false ||
        "$memory_allotment" != true && "$memory_allotment" != false ]]; then
    budget_refuse allocation_domain_invalid "$cores" "$runners" \
      "cpu_sharing_jobs=$cpu_jobs memory_sharing_jobs=$memory_jobs"
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
  if ! resource_policy_valid "$policy"; then
    budget_refuse policy_invalid "$cores" "$runners" "policy=$policy"
    return 1
  fi
  reserved=$(jq -r .reserved_host_cores "$policy")
  maximum=$(jq -r ".${profile}_max_compilers" "$policy")
  reserved_memory=$(jq -r .reserved_host_memory_mb "$policy")
  per_compiler=$(jq -r .memory_mb_per_compiler "$policy")
  if [[ "$cpu_allotment" == true ]]; then
    # Legacy callers already supplied a per-job CPU allotment. Explicit
    # cgroup callers also supply the workers sharing that measured domain.
    [[ $# -ge 8 ]] || cpu_jobs=1
    share=$((cores / cpu_jobs))
  else
    share=$(((cores - reserved) / cpu_jobs))
  fi
  ((share >= 1)) || share=1
  memory_share=$(((memory_mb - reserved_memory) / per_compiler / memory_jobs))
  ((memory_share >= 1)) || memory_share=1
  local build=$share
  ((build <= memory_share)) || build=$memory_share
  ((build <= maximum)) || build=$maximum
  echo "RUST_RESOURCE_BUDGET profile=$profile cores=$cores cpu_allotment=$cpu_allotment memory_mb=$memory_mb memory_allotment=$memory_allotment sharing_jobs=$runners cpu_sharing_jobs=$cpu_jobs memory_sharing_jobs=$memory_jobs cpu_share=$share memory_share=$memory_share build_jobs=$build" >&2
  printf 'build_jobs=%s\ntest_threads=%s\n' "$build" "$share"
}

host_memory_mb() {
  # Total rather than available: the number this budget divides has to be a
  # property of the box, not of whatever happened to be cached when the job
  # started. A reading that moves with page cache would hand two runs of the
  # same workflow two different budgets.
  local cores=${1:-unmeasured} meminfo kb bytes
  # macOS has no /proc. It reports the same property of the box in bytes.
  if [[ "$(uname -s)" == Darwin ]]; then
    bytes=$(sysctl -n hw.memsize 2>/dev/null) || bytes=""
    if [[ ! "$bytes" =~ ^[1-9][0-9]*$ ]]; then
      budget_refuse memory_census_empty "$cores" unmeasured \
        "memory_mb=unmeasured hw_memsize=${bytes:-absent}"
      return 1
    fi
    printf '%s\n' "$((bytes / 1024 / 1024))"
    return 0
  fi
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

host_cpu_cores() {
  # macOS ships no `nproc`.
  if [[ "$(uname -s)" == Darwin ]]; then
    sysctl -n hw.ncpu
  else
    nproc
  fi
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
  # Compare the basename: Linux reports the bare command name, while macOS
  # reports the listener's full path, which an exact match would count as zero.
  count=$(awk '{ n = split($1, part, "/") } part[n] == "Runner.Listener" { count++ } END { print count+0 }' <<< "$census")
  if ((count == 0)); then
    budget_refuse listener_census_empty "$cores" 0 \
      "listener_processes=0 census_status=$census_status"
    return 1
  fi
  printf '%s\n' "$count"
}

# Jobs running on this host now, this one included: each busy runner has one
# Runner.Worker. The budget divides by these rather than by every listener,
# because an idle listener compiles nothing, and dividing by it handed a job on
# a quiet six-runner host one compiler. A count of zero means the census could
# not see this job's own worker, so it falls back to the listener count, which
# is never smaller.
running_local_jobs() {
  local listeners=$1 census count
  census=$(ps -e -o comm= 2>/dev/null) || census=""
  count=$(awk '{ n = split($1, part, "/") } part[n] == "Runner.Worker" { count++ } END { print count+0 }' <<< "$census")
  if ((count < 1)); then
    echo "::warning::RUST_RESOURCE_BUDGET_WORKERS_UNSEEN listeners=$listeners; dividing by listeners" >&2
    count=$listeners
  fi
  ((count <= listeners)) || count=$listeners
  printf '%s\n' "$count"
}

# Read the kernel's unified hierarchy, not nproc's version-dependent quota
# interpretation. Arguments are filesystem roots for the owning fixture; the
# product entry always supplies /proc. A cgroup mount is resolved from that
# process's mount table, so a conventional /sys path is never assumed.
unified_cgroup_path() {
  local proc=$1 pid=$2 path
  path=$(awk -F: '$1 == "0" && $2 == "" {print $3}' "$proc/$pid/cgroup") || return 1
  [[ "$path" == /* && "$path" != *'/../'* && "$path" != */.. && "$path" != *$'\n'* ]] || return 1
  printf '%s\n' "$path"
}

domain_workers() {
  local domain=$1 own=$2 workers=$3 path count=0
  # Worker paths have already been read and validated once, including this
  # job's own worker. An empty domain cannot become measured single-job scope.
  [[ "$own" == "$domain" || "$own" == "${domain%/}/"* ]] || return 1
  while IFS= read -r path; do
    [[ -n "$path" ]] || continue
    if [[ "$path" == "$domain" || "$path" == "${domain%/}/"* ]]; then
      count=$((count + 1))
    fi
  done <<< "$workers"
  ((count >= 1)) || return 1
  printf '%s\n' "$count"
}

linux_resource_limits() {
  local proc=$1 pid=$2 host_cores=$3 host_memory=$4 jobs=$5 environment=$6
  local affinity_cores=${7:-$host_cores}
  local own mount_rows mount_root mountpoint node group workers="" census worker path own_worker_seen=false
  local quota period cpu_quota extra cpu memory memory_bounded memory_max memory_high value controllers domain_jobs cpu_score memory_score reserved reserved_cpu per_compiler
  if [[ ! "$host_cores" =~ ^[1-9][0-9]*$ || ! "$host_memory" =~ ^[1-9][0-9]*$ ||
        ! "$jobs" =~ ^[1-9][0-9]*$ || ! "$affinity_cores" =~ ^[1-9][0-9]*$ ]]; then
    budget_refuse cgroup_inputs_not_positive "$host_cores" "$jobs"; return 1
  fi
  resource_policy_valid "$policy" || {
    budget_refuse policy_invalid "$host_cores" "$jobs"; return 1;
  }
  own=$(unified_cgroup_path "$proc" "$pid") || {
    budget_refuse cgroup_path_unmeasured "$host_cores" "$jobs"; return 1;
  }
  mount_rows=$(awk '$0 ~ / - cgroup2 / {print $4, $5}' "$proc/$pid/mountinfo") || return 1
  [[ -n "$mount_rows" && "$mount_rows" != *$'\n'* ]] || {
    budget_refuse cgroup_mount_unmeasured "$host_cores" "$jobs"; return 1;
  }
  read -r mount_root mountpoint extra <<< "$mount_rows"
  # Escaped/non-root mounts need a separately qualified mapping; refusing them
  # is safer than reporting host capacity as the effective container capacity.
  [[ "$mount_root" == / && "$mountpoint" == /* && "$mountpoint" != *'\'* && -z "$extra" ]] || {
    budget_refuse cgroup_mount_unsupported "$host_cores" "$jobs"; return 1;
  }
  if [[ "$environment" == self-hosted ]]; then
    census=$(ps -e -o pid=,comm=) || return 1
    [[ -n "${census//[[:space:]]/}" ]] || return 1
    while read -r worker; do
      [[ -n "$worker" ]] || continue
      path=$(unified_cgroup_path "$proc" "$worker") || {
        budget_refuse worker_cgroup_unmeasured "$host_cores" "$jobs" "pid=$worker"; return 1;
      }
      workers+="${workers:+$'\n'}$path"
      if [[ "$own" == "$path" || "$own" == "${path%/}/"* ]]; then
        own_worker_seen=true
      fi
    done < <(awk '{n=split($2,p,"/")} p[n]=="Runner.Worker" {print $1}' <<< "$census")
    [[ -n "$workers" ]] || {
      budget_refuse worker_cgroup_census_empty "$host_cores" "$jobs"; return 1;
    }
    [[ "$own_worker_seen" == true ]] || {
      budget_refuse own_worker_cgroup_unmeasured "$host_cores" "$jobs"; return 1;
    }
  else
    workers=$own
  fi
  effective_cores=$host_cores effective_memory_mb=$host_memory
  effective_cpu_jobs=$jobs effective_memory_jobs=$jobs
  effective_cpu_allotment=false effective_memory_allotment=false
  reserved=$(jq -r .reserved_host_memory_mb "$policy") || return 1
  reserved_cpu=$(jq -r .reserved_host_cores "$policy") || return 1
  per_compiler=$(jq -r .memory_mb_per_compiler "$policy") || return 1
  cpu_score=$(((host_cores - reserved_cpu) / jobs))
  if ((affinity_cores < cpu_score)); then
    effective_cores=$affinity_cores effective_cpu_jobs=1 effective_cpu_allotment=true
    cpu_score=$affinity_cores
  fi
  memory_score=$(((host_memory - reserved) / per_compiler / jobs))
  group=$own
  while :; do
    node="${mountpoint%/}$group"
    if [[ "$group" == / && ! -e "$node/cpu.max" && ! -e "$node/memory.max" && ! -e "$node/memory.high" ]]; then
      # The real hierarchy root has no quota files. Establish that the CPU and
      # memory controllers exist before recognizing that kernel-defined case.
      controllers=$(cat "$node/cgroup.controllers") || return 1
      [[ " $controllers " == *' cpu '* && " $controllers " == *' memory '* ]] || {
        budget_refuse cgroup_controllers_unmeasured "$host_cores" "$jobs"; return 1;
      }
      echo 'RUST_RESOURCE_DOMAIN measured=1 domain=/ limits=host_root' >&2
      break
    fi
    read -r quota period extra < "$node/cpu.max" || {
      budget_refuse cgroup_cpu_unmeasured "$host_cores" "$jobs" "domain=$group"; return 1;
    }
    [[ "$period" =~ ^[1-9][0-9]*$ && -z "$extra" &&
       ( "$quota" == max || "$quota" =~ ^[1-9][0-9]*$ ) ]] || {
      budget_refuse cgroup_cpu_malformed "$host_cores" "$jobs" "domain=$group"; return 1;
    }
    domain_jobs=$(domain_workers "$group" "$own" "$workers") || return 1
    cpu_quota=$quota
    if [[ "$quota" != max ]]; then
      cpu=$((quota / period)); ((cpu >= 1)) || cpu=1
      if ((cpu / domain_jobs <= cpu_score)); then
        effective_cores=$cpu effective_cpu_jobs=$domain_jobs effective_cpu_allotment=true
        cpu_score=$((cpu / domain_jobs))
      fi
    fi
    memory=$host_memory memory_bounded=false
    for value in memory.max memory.high; do
      read -r quota extra < "$node/$value" || {
        budget_refuse cgroup_memory_unmeasured "$host_cores" "$jobs" "domain=$group field=$value"; return 1;
      }
      [[ -z "$extra" && ( "$quota" == max || "$quota" =~ ^[1-9][0-9]*$ ) ]] || {
        budget_refuse cgroup_memory_malformed "$host_cores" "$jobs" "domain=$group field=$value"; return 1;
      }
      case "$value" in
        memory.max) memory_max=$quota ;;
        memory.high) memory_high=$quota ;;
      esac
      if [[ "$quota" != max ]]; then
        value=$((quota / 1024 / 1024))
        ((value > 0)) || { budget_refuse cgroup_memory_too_small "$host_cores" "$jobs"; return 1; }
        if ((value < memory)); then
          memory=$value memory_bounded=true
        fi
      fi
    done
    if [[ "$memory_bounded" == true ]] && (( (memory - reserved) / per_compiler / domain_jobs <= memory_score )); then
      effective_memory_mb=$memory effective_memory_jobs=$domain_jobs effective_memory_allotment=true
      memory_score=$(((memory - reserved) / per_compiler / domain_jobs))
    fi
    echo "RUST_RESOURCE_DOMAIN measured=1 domain=$group cpu_max=$cpu_quota cpu_period=$period memory_mb=$memory sharing_jobs=$domain_jobs memory_max_bytes=$memory_max memory_high_bytes=$memory_high" >&2
    [[ "$group" != / ]] || break
    group=${group%/*}; [[ -n "$group" ]] || group=/
  done
}

resource_budget_main() {
  local runners cores memory_mb policy profile=${HARN_BUDGET_PROFILE:-e2e}
  local cpu_allotment=false listeners
  if ! cores=$(host_cpu_cores); then
    budget_refuse cpu_census_failed unmeasured unmeasured
    return 1
  fi
  case "${RUNNER_ENVIRONMENT:-}" in
    github-hosted) runners=1 ;;
    self-hosted)
      listeners=$(online_local_runners "$cores") || return 1
      runners=$(running_local_jobs "$listeners")
      ;;
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
  local memory_allotment=false cpu_jobs=$runners memory_jobs=$runners
  if [[ "$(uname -s)" == Linux ]]; then
    local total_cores
    total_cores=$(nproc --all) || return 1
    [[ "$total_cores" =~ ^[1-9][0-9]*$ ]] || {
      budget_refuse host_cpu_census_unmeasured "$cores" "$runners"; return 1;
    }
    linux_resource_limits /proc self "$total_cores" "$memory_mb" "$runners" "$RUNNER_ENVIRONMENT" "$cores" || return 1
    cores=$effective_cores memory_mb=$effective_memory_mb
    cpu_allotment=$effective_cpu_allotment memory_allotment=$effective_memory_allotment
    cpu_jobs=$effective_cpu_jobs memory_jobs=$effective_memory_jobs
  fi
  rust_resource_budget "$policy" "$cores" "$runners" "$profile" "$memory_mb" "$cpu_allotment" \
    "$memory_allotment" "$cpu_jobs" "$memory_jobs" \
    >> "${GITHUB_OUTPUT:?}"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  resource_budget_main
fi
