#!/usr/bin/env bash
# Names where a self-hosted-eligible CI job is about to land, before dispatch.
#
# The routing expression on a job can only choose; it cannot say why, and it
# treats an unreadable capacity census exactly like a census that reported no
# capacity. Both fall through to a hosted runner, so a broken or retired
# census silently downgrades the tier the job exists to use and leaves nothing
# in the log to attribute it to. This runs first and names the decision, so a
# route is only ever chosen over counts that were actually observed, or over
# a census failure that is said out loud.
#
# A census that cannot be read routes hosted as a named fallback, never as a
# skip. The job the decision serves is required proof, and a fleet-census
# outage must not take that proof away or turn main red. What the fallback
# must not do is pass for a measurement: its decision line carries
# `fallback=true` and the reason, and it emits a `<LABEL>_FALLBACK` warning
# annotation, so a broken census reads as a count of fallbacks, not as an
# empty pool.
#
# It reads the same fleet-evacuation switch the routing expression reads, for
# the same reason: a script that reported the owned census while the switch
# sent the job to elastic paid capacity would name a route the job never took.
#
# Two callers, two shapes, one decision procedure:
#
#   CAPACITY_ROUTED_EVENT  the events allowed onto owned capacity,
#                          space-separated. The slow E2E suite routes main
#                          pushes; the Rust workspace producer routes pull
#                          requests; the shared CLI producer routes pull
#                          requests and merge groups. Every other event is
#                          named and sent hosted.
#   CAPACITY_POOL          which pool of the census answers for this job.
#   CAPACITY_LABEL         the log prefix, so two jobs in one run stay
#                          attributable to their own decision.
#   CAPACITY_BUSY_ROUTE    the route a measured, fully busy pool takes:
#                          `hosted` (the default) or `overflow`. A job whose
#                          paid rung is a shared vendor quota names `overflow`
#                          to reach GitHub's runner, which that quota cannot
#                          starve.
set -euo pipefail

CAPACITY_POOL=${CAPACITY_POOL:-linux_big}
CAPACITY_ROUTED_EVENT=${CAPACITY_ROUTED_EVENT:-push}
CAPACITY_LABEL=${CAPACITY_LABEL:-RUNNER_CAPACITY_DECISION}
CAPACITY_BUSY_ROUTE=${CAPACITY_BUSY_ROUTE:-hosted}
case "$CAPACITY_BUSY_ROUTE" in
  hosted | overflow) ;;
  *)
    echo "CAPACITY_BUSY_ROUTE must be hosted or overflow, not '$CAPACITY_BUSY_ROUTE'" >&2
    exit 2
    ;;
esac

# Whether an event is one of the space-separated routed events.
runner_capacity_routed_event() {
  local routed
  for routed in $CAPACITY_ROUTED_EVENT; do
    [[ "$1" == "$routed" ]] && return 0
  done
  return 1
}

runner_capacity_fallback() {
  local reason=$1 event=$2
  shift 2
  echo "::warning::${CAPACITY_LABEL}_FALLBACK reason=$reason $*" >&2
  echo "${CAPACITY_LABEL} event=$event route=hosted reason=$reason fallback=true $*"
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
  if ! runner_capacity_routed_event "$event"; then
    local routed=${CAPACITY_ROUTED_EVENT// /_or_}
    echo "${CAPACITY_LABEL} event=$event route=hosted reason=event_is_not_${routed} pool=$pool carriers=not_consulted"
    return 0
  fi
  if [[ -n "${disabled//[[:space:]]/}" ]]; then
    echo "${CAPACITY_LABEL} event=$event route=hosted reason=owned_routing_retired pool=$pool carriers=not_consulted"
    return 0
  fi
  # An absent census is not an empty pool. Fall back by name rather than let
  # it read as one, and carry what was observed so the fallback is
  # attributable.
  if [[ -z "${capacity//[[:space:]]/}" ]]; then
    runner_capacity_fallback capacity_census_missing "$event" \
      "pool=$pool carriers=unmeasured capacity_bytes=${#capacity}"
    return 0
  fi
  if ! jq -e . <<< "$capacity" >/dev/null 2>&1; then
    runner_capacity_fallback capacity_census_unreadable "$event" \
      "pool=$pool carriers=unmeasured capacity_bytes=${#capacity}"
    return 0
  fi
  # A census that does not mention the pool has not measured it. That is a
  # different fact from a pool it measured and found empty.
  if ! jq -e --arg pool "$pool" \
    'has($pool) and (.[$pool].online | type == "number")' <<< "$capacity" >/dev/null; then
    runner_capacity_fallback capacity_pool_absent "$event" \
      "pool=$pool carriers=unmeasured pools=$(jq -r 'keys | join(",")' <<< "$capacity")"
    return 0
  fi
  online=$(jq -r --arg pool "$pool" '.[$pool].online' <<< "$capacity")
  idle=$(jq -r --arg pool "$pool" '.[$pool].idle // "unreported"' <<< "$capacity")
  if ((online < 1)); then
    # A retired pool reports zero carriers. Say so, and route hosted. The job
    # must never be sent to a label nothing is listening on.
    echo "${CAPACITY_LABEL} event=$event route=hosted reason=pool_reported_zero_carriers pool=$pool carriers=0 idle=$idle"
    return 0
  fi
  if [[ "$idle" =~ ^[0-9]+$ ]] && ((idle < 1)); then
    # Every carrier is busy. Queueing behind them cost same-repository pull
    # requests 40-60 minutes on 2026-10-01, long enough for the producer's
    # consumers to give up waiting, so a fully busy pool routes hosted (#9086).
    # An unreported idle count keeps the owned route: absence is not a
    # measured zero. A caller that names the overflow route leaves its shared
    # vendor rung for GitHub's runner here, because a busy owned pool and a
    # saturated vendor quota arrive together (2026-10-02, run 37042288569).
    echo "${CAPACITY_LABEL} event=$event route=${CAPACITY_BUSY_ROUTE} reason=pool_fully_busy pool=$pool carriers=$online idle=0"
    return 0
  fi
  # An idle carrier is not a compile budget. A job's share of its host shrinks
  # with every job already running there, and GitHub hands the job to any
  # idle carrier in the pool, not to the quietest host. So when the census
  # breaks the pool down by host, every host that could receive the job must
  # have room: with the job added, at most half its runners busy, which on
  # each owned host leaves the job at least twice the compilers it would get
  # on a fully busy one. A census without the breakdown keeps the pool rule.
  local hosts saturated
  if jq -e --arg pool "$pool" '.[$pool] | has("hosts")' <<< "$capacity" >/dev/null; then
    hosts=$(jq -c --arg pool "$pool" '.[$pool].hosts' <<< "$capacity")
    if ! jq -e --arg idle "$idle" 'def count: type == "number" and . >= 0 and floor == .;
        type == "object" and length > 0 and all(.[];
          type == "object" and (.online | count) and (.busy | count)
          and .online > 0 and (.idle_big | count) and .busy <= .online
          and .idle_big <= (.online - .busy))
        and (if ($idle | test("^[0-9]+$"))
          then ([.[].idle_big] | add) == ($idle | tonumber) else true end)' \
        <<< "$hosts" > /dev/null 2>&1; then
      runner_capacity_fallback capacity_hosts_unreadable "$event" \
        "pool=$pool carriers=$online idle=$idle host_counts=unmeasured"
      return 0
    fi
    saturated=$(jq -r '[to_entries[] | select(.value.idle_big > 0)
      | select((.value.busy + 1) * 2 > .value.online)
      | "\(.key):\(.value.busy)/\(.value.online)"] | join(",")' <<< "$hosts")
    if [[ -n $saturated ]]; then
      echo "${CAPACITY_LABEL} event=$event route=hosted reason=owned_hosts_saturated pool=$pool carriers=$online idle=$idle busy_hosts=$saturated"
      return 0
    fi
  fi
  echo "${CAPACITY_LABEL} event=$event route=owned pool=$pool carriers=$online idle=$idle"
}

# Which paid runner a hosted route lands on, for callers whose ladder has a
# vendor rung before GitHub's. It reads the same variable the ladder reads, so
# the line names the runner the job takes rather than the class it falls in.
# Callers without CAPACITY_VENDOR_RUNNER keep the line they had.
runner_capacity_paid_runner() {
  local route=$1
  if [[ "$route" == overflow ]]; then
    [[ -n "${CAPACITY_HOSTED_RUNNER:-}" ]] || return 0
    printf ' paid_runner=%s paid_provider=github' "$CAPACITY_HOSTED_RUNNER"
    return 0
  fi
  [[ "$route" == hosted && -n "${CAPACITY_VENDOR_RUNNER:-}" ]] || return 0
  if [[ "${PAID_LINUX_PROVIDER:-}" == github ]]; then
    printf ' paid_runner=%s paid_provider=github' "${CAPACITY_HOSTED_RUNNER:?}"
  else
    printf ' paid_runner=%s paid_provider=%s' "$CAPACITY_VENDOR_RUNNER" \
      "${PAID_LINUX_PROVIDER:-ubicloud_default}"
  fi
}

runner_capacity_main() {
  local line route fallback
  line=$(runner_capacity_decision \
    "${EVENT_NAME:-}" "${SELFHOSTED_DISABLED:-}" "${RUNNER_CAPACITY:-}" \
    "${FLEET_EVACUATION:-}") || return 1
  route=${line##*route=}
  route=${route%% *}
  # The evacuation switch sends the job to Blacksmith, not to the paid rung.
  if [[ "${FLEET_EVACUATION:-}" != true ]]; then
    line+=$(runner_capacity_paid_runner "$route")
  fi
  echo "$line" >&2
  route=${line##*route=}
  route=${route%% *}
  fallback=false
  [[ "$line" == *" fallback=true"* ]] && fallback=true
  printf 'route=%s\nfallback=%s\n' "$route" "$fallback" >> "${GITHUB_OUTPUT:?}"
  # The job summary is where a reader looks for why a job landed where it did.
  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    # shellcheck disable=SC2016 # Literal Markdown code spans.
    printf '%s: route `%s`\n\n`%s`\n' "$CAPACITY_LABEL" "$route" "$line" >> "$GITHUB_STEP_SUMMARY"
  fi
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  runner_capacity_main
fi
