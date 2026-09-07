#!/usr/bin/env bash
# Names where a self-hosted-eligible CI job is about to land, before dispatch.
#
# The routing expression on a job can only choose; it cannot say why, and it
# treats an unreadable capacity census exactly like a census that reported no
# capacity. Both fall through to a hosted runner, so a broken or retired
# census silently downgrades the tier the job exists to use and leaves nothing
# in the log to attribute it to. This runs first and refuses by name when the
# census cannot be read, so the choice is only ever made over counts that were
# actually observed.
#
# It reads the same fleet-evacuation switch the routing expression reads, for
# the same reason: a script that reported the owned census while the switch
# sent the job to elastic paid capacity would name a route the job never took.
#
# Two callers, two shapes, one decision procedure:
#
#   CAPACITY_ROUTED_EVENT  the single event allowed onto owned capacity. The
#                          slow E2E suite routes main pushes; the Rust
#                          workspace producer routes pull requests. Every
#                          other event is named and sent hosted.
#   CAPACITY_POOL          which pool of the census answers for this job.
#   CAPACITY_LABEL         the log prefix, so two jobs in one run stay
#                          attributable to their own decision.
set -euo pipefail

CAPACITY_POOL=${CAPACITY_POOL:-linux_big}
CAPACITY_ROUTED_EVENT=${CAPACITY_ROUTED_EVENT:-push}
CAPACITY_LABEL=${CAPACITY_LABEL:-RUNNER_CAPACITY_DECISION}

runner_capacity_refuse() {
  local reason=$1
  shift
  echo "::error::${CAPACITY_LABEL}_UNMEASURED reason=$reason $*" >&2
  return 1
}

runner_capacity_decision() {
  local event=$1 disabled=$2 capacity=$3 evacuate=${4:-} pool=$CAPACITY_POOL online idle
  # The fleet-evacuation switch is read first because the routing expression
  # reads it first: when it is on, every event goes to elastic paid capacity
  # regardless of what the owned census says. Reporting the census answer here
  # would name a route the job never takes, which is the divergence this
  # script exists to close.
  if [[ "$evacuate" == true ]]; then
    echo "${CAPACITY_LABEL} event=$event route=hosted reason=fleet_evacuation_switch_on pool=$pool carriers=1"
    return 0
  fi
  if [[ "$event" != "$CAPACITY_ROUTED_EVENT" ]]; then
    echo "${CAPACITY_LABEL} event=$event route=hosted reason=event_is_not_${CAPACITY_ROUTED_EVENT} pool=$pool carriers=not_consulted"
    return 0
  fi
  if [[ -n "${disabled//[[:space:]]/}" ]]; then
    echo "${CAPACITY_LABEL} event=$event route=hosted reason=owned_routing_retired pool=$pool carriers=not_consulted"
    return 0
  fi
  # An absent census is not an empty pool. Refuse rather than let it read as
  # one, and carry what was observed so the refusal is attributable.
  if [[ -z "${capacity//[[:space:]]/}" ]]; then
    runner_capacity_refuse capacity_census_missing \
      "event=$event pool=$pool carriers=unmeasured capacity_bytes=${#capacity}"
    return 1
  fi
  if ! jq -e . <<< "$capacity" >/dev/null 2>&1; then
    runner_capacity_refuse capacity_census_unreadable \
      "event=$event pool=$pool carriers=unmeasured capacity_bytes=${#capacity}"
    return 1
  fi
  # A census that does not mention the pool has not measured it. That is a
  # different fact from a pool it measured and found empty.
  if ! jq -e --arg pool "$pool" \
    'has($pool) and (.[$pool].online | type == "number")' <<< "$capacity" >/dev/null; then
    runner_capacity_refuse capacity_pool_absent \
      "event=$event pool=$pool carriers=unmeasured pools=$(jq -r 'keys | join(",")' <<< "$capacity")"
    return 1
  fi
  online=$(jq -r --arg pool "$pool" '.[$pool].online' <<< "$capacity")
  idle=$(jq -r --arg pool "$pool" '.[$pool].idle // "unreported"' <<< "$capacity")
  if ((online < 1)); then
    # A retired pool reports zero carriers. Say so, and route hosted. The job
    # must never be sent to a label nothing is listening on.
    echo "${CAPACITY_LABEL} event=$event route=hosted reason=pool_reported_zero_carriers pool=$pool carriers=0 idle=$idle"
    return 0
  fi
  echo "${CAPACITY_LABEL} event=$event route=owned pool=$pool carriers=$online idle=$idle"
}

runner_capacity_main() {
  local line route
  line=$(runner_capacity_decision \
    "${EVENT_NAME:-}" "${SELFHOSTED_DISABLED:-}" "${RUNNER_CAPACITY:-}" \
    "${FLEET_EVACUATION:-}") || return 1
  echo "$line" >&2
  route=${line##*route=}
  route=${route%% *}
  printf 'route=%s\n' "$route" >> "${GITHUB_OUTPUT:?}"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  runner_capacity_main
fi
