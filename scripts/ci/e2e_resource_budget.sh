#!/usr/bin/env bash
# Bootstrap budget: runs before a source-built CLI is available.
set -euo pipefail

e2e_resource_budget() {
  local policy=$1 cores=$2 runners=$3 reserved maximum share
  if [[ ! "$cores" =~ ^[1-9][0-9]*$ || ! "$runners" =~ ^[1-9][0-9]*$ ]]; then
    echo '::error::E2E_RESOURCE_BUDGET_UNMEASURED: CPU and runner counts must be positive' >&2
    return 1
  fi
  if ! jq -e '(.schema_version == 1) and
      ([.reserved_host_cores, .e2e_max_compilers] |
      all(type == "number" and . >= 1 and . == floor))' "$policy" >/dev/null; then
    echo '::error::E2E_RESOURCE_BUDGET_UNMEASURED: invalid resource policy' >&2
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
  local cores=${1:-unmeasured} listeners count census_status
  if listeners=$(ps -C Runner.Listener -o pid=); then
    census_status=0
  else
    census_status=$?
    echo "::error::E2E_RESOURCE_BUDGET_UNMEASURED reason=listener_census_failed cpu_cores=$cores online_local_runners=unmeasured listener_processes=unmeasured census_status=$census_status" >&2
    return 1
  fi
  count=$(awk 'NF { count++ } END { print count+0 }' <<< "$listeners")
  if ((count == 0)); then
    echo "::error::E2E_RESOURCE_BUDGET_UNMEASURED reason=listener_census_empty cpu_cores=$cores online_local_runners=0 listener_processes=0 census_status=$census_status" >&2
    return 1
  fi
  printf '%s\n' "$count"
}

e2e_resource_main() {
  local runners cores policy
  if ! cores=$(nproc); then
    echo '::error::E2E_RESOURCE_BUDGET_UNMEASURED reason=cpu_census_failed cpu_cores=unmeasured online_local_runners=unmeasured' >&2
    return 1
  fi
  case "${RUNNER_ENVIRONMENT:-}" in
    github-hosted) runners=1 ;;
    self-hosted) runners=$(e2e_online_local_runners "$cores") || return 1 ;;
    *) echo '::error::E2E_RESOURCE_BUDGET_UNMEASURED: runner environment missing' >&2; return 1 ;;
  esac
  policy="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/rust-resource-policy.json"
  e2e_resource_budget "$policy" "$cores" "$runners" >> "${GITHUB_OUTPUT:?}"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  e2e_resource_main
fi
