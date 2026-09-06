#!/usr/bin/env bash
# Names where the slow E2E suite is about to land, before it is dispatched.
#
# The routing expression on the job can only choose; it cannot say why, and it
# treats an unreadable capacity census exactly like a census that reported no
# capacity. Both fall through to a hosted runner, so a broken or retired
# census silently downgrades the tier this suite exists to provide and leaves
# nothing in the log to attribute it to. This runs first and refuses by name
# when the census cannot be read, so the choice is only ever made over counts
# that were actually observed.
#
# It reads the same fleet-evacuation switch the routing expression reads, for
# the same reason: a script that reported the owned census while the switch
# sent the job to elastic paid capacity would name a route the job never took.
set -euo pipefail

E2E_CAPACITY_POOL=${E2E_CAPACITY_POOL:-linux_big}

e2e_capacity_refuse() {
  local reason=$1
  shift
  echo "::error::E2E_RUNNER_CAPACITY_UNMEASURED reason=$reason $*" >&2
  return 1
}

e2e_runner_capacity_decision() {
  local event=$1 disabled=$2 capacity=$3 evacuate=${4:-} pool=$E2E_CAPACITY_POOL online idle
  # The fleet-evacuation switch is read first because the routing expression
  # reads it first: when it is on, every event goes to elastic paid capacity
  # regardless of what the owned census says. Reporting the census answer here
  # would name a route the job never takes, which is the divergence this
  # script exists to close.
  if [[ "$evacuate" == true ]]; then
    echo "E2E_RUNNER_CAPACITY event=$event route=hosted reason=fleet_evacuation_switch_on pool=$pool carriers=1"
    return 0
  fi
  if [[ "$event" != push ]]; then
    echo "E2E_RUNNER_CAPACITY event=$event route=hosted reason=event_is_not_a_main_push pool=$pool carriers=not_consulted"
    return 0
  fi
  if [[ -n "${disabled//[[:space:]]/}" ]]; then
    echo "E2E_RUNNER_CAPACITY event=$event route=hosted reason=owned_routing_retired pool=$pool carriers=not_consulted"
    return 0
  fi
  # An absent census is not an empty pool. Refuse rather than let it read as
  # one, and carry what was observed so the refusal is attributable.
  if [[ -z "${capacity//[[:space:]]/}" ]]; then
    e2e_capacity_refuse capacity_census_missing \
      "event=$event pool=$pool carriers=unmeasured capacity_bytes=${#capacity}"
    return 1
  fi
  if ! jq -e . <<< "$capacity" >/dev/null 2>&1; then
    e2e_capacity_refuse capacity_census_unreadable \
      "event=$event pool=$pool carriers=unmeasured capacity_bytes=${#capacity}"
    return 1
  fi
  # A census that does not mention the pool has not measured it. That is a
  # different fact from a pool it measured and found empty.
  if ! jq -e --arg pool "$pool" \
    'has($pool) and (.[$pool].online | type == "number")' <<< "$capacity" >/dev/null; then
    e2e_capacity_refuse capacity_pool_absent \
      "event=$event pool=$pool carriers=unmeasured pools=$(jq -r 'keys | join(",")' <<< "$capacity")"
    return 1
  fi
  online=$(jq -r --arg pool "$pool" '.[$pool].online' <<< "$capacity")
  idle=$(jq -r --arg pool "$pool" '.[$pool].idle // "unreported"' <<< "$capacity")
  if ((online < 1)); then
    # A retired pool reports zero carriers. Say so, and route hosted. The job
    # must never be sent to a label nothing is listening on.
    echo "E2E_RUNNER_CAPACITY event=$event route=hosted reason=pool_reported_zero_carriers pool=$pool carriers=0 idle=$idle"
    return 0
  fi
  echo "E2E_RUNNER_CAPACITY event=$event route=owned pool=$pool carriers=$online idle=$idle"
}

e2e_runner_capacity_main() {
  local line route
  line=$(e2e_runner_capacity_decision \
    "${EVENT_NAME:-}" "${SELFHOSTED_DISABLED:-}" "${RUNNER_CAPACITY:-}" \
    "${FLEET_EVACUATION:-}") || return 1
  echo "$line" >&2
  route=${line##*route=}
  route=${route%% *}
  printf 'route=%s\n' "$route" >> "${GITHUB_OUTPUT:?}"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  e2e_runner_capacity_main
fi
