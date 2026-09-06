#!/usr/bin/env bash
# Bootstrap budget: runs before a source-built CLI is available.
set -euo pipefail

# Every refusal in this file is named and carries what was actually observed,
# so an unmeasurable host is never mistaken for a measured one.
e2e_budget_refuse() {
  local reason=$1 cores=$2 runners=$3
  shift 3
  echo "::error::E2E_RESOURCE_BUDGET_UNMEASURED reason=$reason cpu_cores=$cores online_local_runners=$runners${*:+ $*}" >&2
  return 1
}

e2e_resource_budget() {
  local policy=$1 cores=$2 runners=$3 reserved maximum share
  if [[ ! "$cores" =~ ^[1-9][0-9]*$ || ! "$runners" =~ ^[1-9][0-9]*$ ]]; then
    e2e_budget_refuse census_not_positive "$cores" "$runners"
    return 1
  fi
  if ! jq -e '(.schema_version == 1) and
      ([.reserved_host_cores, .e2e_max_compilers] |
      all(type == "number" and . >= 1 and . == floor))' "$policy" >/dev/null; then
    e2e_budget_refuse policy_invalid "$cores" "$runners" "policy=$policy"
    return 1
  fi
  reserved=$(jq -r .reserved_host_cores "$policy")
  maximum=$(jq -r .e2e_max_compilers "$policy")
  share=$(((cores - reserved) / runners))
  ((share >= 1)) || share=1
  local build=$share
  ((build <= maximum)) || build=$maximum
  echo "E2E_RESOURCE_BUDGET cores=$cores online_local_runners=$runners cpu_share=$share build_jobs=$build" >&2
  printf 'build_jobs=%s\ntest_threads=%s\n' "$build" "$share"
}

e2e_online_local_runners() {
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
    e2e_budget_refuse listener_census_failed "$cores" unmeasured \
      "listener_processes=unmeasured census_status=$census_status"
    return 1
  fi
  count=$(awk '$1 == "Runner.Listener" { count++ } END { print count+0 }' <<< "$census")
  if ((count == 0)); then
    e2e_budget_refuse listener_census_empty "$cores" 0 \
      "listener_processes=0 census_status=$census_status"
    return 1
  fi
  printf '%s\n' "$count"
}

e2e_resource_main() {
  local runners cores policy
  if ! cores=$(nproc); then
    e2e_budget_refuse cpu_census_failed unmeasured unmeasured
    return 1
  fi
  case "${RUNNER_ENVIRONMENT:-}" in
    github-hosted) runners=1 ;;
    self-hosted) runners=$(e2e_online_local_runners "$cores") || return 1 ;;
    *)
      e2e_budget_refuse runner_environment_missing "$cores" unmeasured \
        "runner_environment=${RUNNER_ENVIRONMENT:-unset}"
      return 1
      ;;
  esac
  policy="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/rust-resource-policy.json"
  e2e_resource_budget "$policy" "$cores" "$runners" >> "${GITHUB_OUTPUT:?}"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  e2e_resource_main
fi
