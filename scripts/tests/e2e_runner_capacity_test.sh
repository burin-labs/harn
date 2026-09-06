#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
source "$root/scripts/ci/e2e_runner_capacity.sh"
diagnostic=$(mktemp "${TMPDIR:-/tmp}/harn-e2e-runner-capacity.XXXXXX")
trap 'rm -f "$diagnostic"' EXIT

refuses() {
  local reason=$1
  shift
  if e2e_runner_capacity_decision "$@" 2>"$diagnostic"; then
    echo "expected refusal $reason, got a decision" >&2
    exit 1
  fi
  grep -q "E2E_RUNNER_CAPACITY_UNMEASURED reason=$reason " "$diagnostic"
  grep -q 'carriers=unmeasured' "$diagnostic"
}

measured='{"linux_big":{"online":3,"idle":1}}'
retired='{"linux_big":{"online":0,"idle":0}}'

# A measured pool with carriers takes the owned route and says how many.
[[ $(e2e_runner_capacity_decision push '' "$measured") == \
  'E2E_RUNNER_CAPACITY event=push route=owned pool=linux_big carriers=3 idle=1' ]]

# A retired pool reports zero and routes hosted. It must never be sent to a
# label nothing is listening on, and the observed zero must appear.
decision=$(e2e_runner_capacity_decision push '' "$retired")
[[ $decision == \
  'E2E_RUNNER_CAPACITY event=push route=hosted reason=pool_reported_zero_carriers pool=linux_big carriers=0 idle=0' ]]
[[ $decision != *route=owned* ]]

# Every non-push event keeps hosted runners without consulting the census.
[[ $(e2e_runner_capacity_decision pull_request '' '') == \
  'E2E_RUNNER_CAPACITY event=pull_request route=hosted reason=event_is_not_a_main_push pool=linux_big carriers=not_consulted' ]]
[[ $(e2e_runner_capacity_decision schedule '' "$measured") == *route=hosted* ]]

# Routing switched off org-wide is a decision, not a missing measurement.
[[ $(e2e_runner_capacity_decision push 'retired' "$measured") == \
  'E2E_RUNNER_CAPACITY event=push route=hosted reason=owned_routing_retired pool=linux_big carriers=not_consulted' ]]

# An unmeasurable census must refuse by name and never read as an empty pool.
refuses capacity_census_missing push '' ''
refuses capacity_census_missing push '' '   '
refuses capacity_census_unreadable push '' 'not json'
refuses capacity_census_unreadable push '' '{"linux_big":'
refuses capacity_pool_absent push '' '{"macos_big":{"online":2}}'
refuses capacity_pool_absent push '' '{"linux_big":{"idle":0}}'
refuses capacity_pool_absent push '' '{"linux_big":{"online":"3"}}'

# The pool-absent refusal names which pools the census did report, so the
# difference between a renamed pool and a dead census is readable.
e2e_runner_capacity_decision push '' '{"macos_big":{"online":2}}' 2>"$diagnostic" || true
grep -q 'pools=macos_big' "$diagnostic"

echo 'E2E runner capacity: owned, retired, non-push, retired-routing and unmeasurable-census controls passed'
