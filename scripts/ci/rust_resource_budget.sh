#!/usr/bin/env bash
# Bootstrap budget: runs before a source-built CLI is available.
#
# One owner for "how many rustc processes may this job start". A literal in a
# workflow answers that question for the box someone had in mind when they typed
# it, and is wrong on every other box: the same number oversubscribes a small
# hosted runner and leaves an owned machine idle. The count is measured here
# from the host and divided by the listeners actually sharing it, then capped
# per profile, because peak memory rather than cores is what bounds a Rust
# compile.
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
  local policy=$1 cores=$2 runners=$3 profile=${4:-e2e} reserved maximum share
  if [[ ! "$cores" =~ ^[1-9][0-9]*$ || ! "$runners" =~ ^[1-9][0-9]*$ ]]; then
    budget_refuse census_not_positive "$cores" "$runners"
    return 1
  fi
  if [[ "$profile" != "e2e" && "$profile" != "producer" ]]; then
    budget_refuse profile_unknown "$cores" "$runners" "profile=$profile"
    return 1
  fi
  if ! jq -e '(.schema_version == 2) and
      ([.reserved_host_cores, .e2e_max_compilers, .producer_max_compilers] |
      all(type == "number" and . >= 1 and . == floor))' "$policy" >/dev/null; then
    budget_refuse policy_invalid "$cores" "$runners" "policy=$policy"
    return 1
  fi
  reserved=$(jq -r .reserved_host_cores "$policy")
  maximum=$(jq -r ".${profile}_max_compilers" "$policy")
  share=$(((cores - reserved) / runners))
  ((share >= 1)) || share=1
  local build=$share
  ((build <= maximum)) || build=$maximum
  echo "RUST_RESOURCE_BUDGET profile=$profile cores=$cores online_local_runners=$runners cpu_share=$share build_jobs=$build" >&2
  printf 'build_jobs=%s\ntest_threads=%s\n' "$build" "$share"
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
  local runners cores policy profile=${HARN_BUDGET_PROFILE:-e2e}
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
  policy="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/rust-resource-policy.json"
  rust_resource_budget "$policy" "$cores" "$runners" "$profile" >> "${GITHUB_OUTPUT:?}"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  resource_budget_main
fi
