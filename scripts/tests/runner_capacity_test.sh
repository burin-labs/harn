#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
source "$root/scripts/ci/runner_capacity.sh"
diagnostic=$(mktemp "${TMPDIR:-/tmp}/harn-e2e-runner-capacity.XXXXXX")
trap 'rm -f "$diagnostic"' EXIT

# A census that cannot be read routes hosted as a named fallback: the decision
# succeeds (the required job still runs), the line says fallback=true with the
# reason, and a FALLBACK warning annotation makes it countable. It must never
# read as a measured empty pool.
falls_back() {
  local reason=$1 decision
  shift
  if ! decision=$(runner_capacity_decision "$@" 2>"$diagnostic"); then
    echo "expected fallback $reason, got a refusal" >&2
    exit 1
  fi
  [[ $decision == *"route=hosted reason=$reason fallback=true "* ]]
  [[ $decision == *carriers=unmeasured* ]]
  [[ $decision != *route=owned* ]]
  grep -q "::warning::RUNNER_CAPACITY_DECISION_FALLBACK reason=$reason " "$diagnostic"
}

measured='{"linux_big":{"online":3,"idle":1}}'
retired='{"linux_big":{"online":0,"idle":0}}'

# A measured pool with carriers takes the owned route and says how many.
[[ $(runner_capacity_decision push '' "$measured") == \
  'RUNNER_CAPACITY_DECISION event=push route=owned pool=linux_big carriers=3 idle=1' ]]

# A retired pool reports zero and routes hosted. It must never be sent to a
# label nothing is listening on, and the observed zero must appear.
decision=$(runner_capacity_decision push '' "$retired")
[[ $decision == \
  'RUNNER_CAPACITY_DECISION event=push route=hosted reason=pool_reported_zero_carriers pool=linux_big carriers=0 idle=0' ]]
[[ $decision != *route=owned* ]]

# A fully busy pool routes hosted by name rather than queueing (#9086), and an
# unreported idle count is not a measured zero, so it keeps the owned route.
busy='{"linux_big":{"online":3,"idle":0}}'
[[ $(runner_capacity_decision push '' "$busy") == \
  'RUNNER_CAPACITY_DECISION event=push route=hosted reason=pool_fully_busy pool=linux_big carriers=3 idle=0' ]]
unreported='{"linux_big":{"online":3}}'
[[ $(runner_capacity_decision push '' "$unreported") == \
  'RUNNER_CAPACITY_DECISION event=push route=owned pool=linux_big carriers=3 idle=unreported' ]]

# The main entry point names the decision in the job summary as well as the log.
summary=$(mktemp "${TMPDIR:-/tmp}/harn-capacity-summary.XXXXXX")
outputs=$(mktemp "${TMPDIR:-/tmp}/harn-capacity-outputs.XXXXXX")
EVENT_NAME=push SELFHOSTED_DISABLED='' RUNNER_CAPACITY="$busy" FLEET_EVACUATION='' \
  GITHUB_OUTPUT="$outputs" GITHUB_STEP_SUMMARY="$summary" runner_capacity_main 2>/dev/null
grep -qx 'route=hosted' "$outputs"
# shellcheck disable=SC2016 # Literal Markdown code span.
grep -q 'route `hosted`' "$summary"
grep -q 'reason=pool_fully_busy' "$summary"
rm -f "$summary" "$outputs"

# Every non-push event keeps hosted runners without consulting the census.
[[ $(runner_capacity_decision pull_request '' '') == \
  'RUNNER_CAPACITY_DECISION event=pull_request route=hosted reason=event_is_not_push pool=linux_big carriers=not_consulted' ]]
[[ $(runner_capacity_decision schedule '' "$measured") == *route=hosted* ]]

# The fleet-evacuation switch takes every event to elastic paid capacity, so
# the decision line must name the hosted route and its single carrier rather
# than the owned census the job will not use.
[[ $(runner_capacity_decision push '' "$measured" true) == \
  'RUNNER_CAPACITY_DECISION event=push route=hosted reason=fleet_evacuation_switch_on pool=linux_big carriers=1' ]]
[[ $(runner_capacity_decision pull_request '' '' true) == \
  'RUNNER_CAPACITY_DECISION event=pull_request route=hosted reason=fleet_evacuation_switch_on pool=linux_big carriers=1' ]]

# The switch is only the literal `true`. An unset or otherwise-valued switch
# must leave the census answer standing, or a typo in the workflow would
# silently evacuate the fleet.
[[ $(runner_capacity_decision push '' "$measured" '') == *route=owned* ]]
[[ $(runner_capacity_decision push '' "$measured" 'false') == *route=owned* ]]
[[ $(runner_capacity_decision push '' "$measured" 'TRUE') == *route=owned* ]]

# Routing switched off org-wide is a decision, not a missing measurement.
[[ $(runner_capacity_decision push 'retired' "$measured") == \
  'RUNNER_CAPACITY_DECISION event=push route=hosted reason=owned_routing_retired pool=linux_big carriers=not_consulted' ]]

# An unmeasurable census falls back by name and never reads as an empty pool.
falls_back capacity_census_missing push '' ''
falls_back capacity_census_missing push '' '   '
falls_back capacity_census_unreadable push '' 'not json'
falls_back capacity_census_unreadable push '' '{"linux_big":'
falls_back capacity_pool_absent push '' '{"macos_big":{"online":2}}'
falls_back capacity_pool_absent push '' '{"linux_big":{"idle":0}}'
falls_back capacity_pool_absent push '' '{"linux_big":{"online":"3"}}'

# A measured zero is not a fallback: only the unmeasured census carries it.
[[ $(runner_capacity_decision push '' "$retired") != *fallback=* ]]

# The pool-absent fallback names which pools the census did report, so the
# difference between a renamed pool and a dead census is readable.
runner_capacity_decision push '' '{"macos_big":{"online":2}}' 2>"$diagnostic" >/dev/null
grep -q 'pools=macos_big' "$diagnostic"

# The routed event is a parameter, not a constant. The Rust workspace producer
# routes pull requests onto owned capacity and sends main pushes hosted, which
# is the exact inverse of the E2E suite, and a decision procedure that hardcoded
# `push` would name the wrong route for it.
CAPACITY_ROUTED_EVENT=pull_request
[[ $(runner_capacity_decision pull_request '' "$measured") == *route=owned* ]]
[[ $(runner_capacity_decision push '' "$measured") == \
  *"route=hosted reason=event_is_not_pull_request"* ]]
CAPACITY_ROUTED_EVENT=push

# The label is a parameter too, so two capacity decisions in one workflow run
# stay attributable to the job that made them.
CAPACITY_LABEL=RUST_PRODUCER_CAPACITY
[[ $(runner_capacity_decision push '' "$measured") == "RUST_PRODUCER_CAPACITY "* ]]
CAPACITY_LABEL=RUNNER_CAPACITY_DECISION

# A hosted decision names the paid runner the ladder picks: Ubicloud unless the
# provider variable says github. Owned routes, evacuations, and callers with no
# vendor rung carry no paid runner at all.
paid_line() {
  local output
  output="$(mktemp)"
  GITHUB_OUTPUT="$output" GITHUB_STEP_SUMMARY='' EVENT_NAME=$1 SELFHOSTED_DISABLED='' \
    RUNNER_CAPACITY=$2 FLEET_EVACUATION=${3:-} PAID_LINUX_PROVIDER=${4:-} \
    CAPACITY_VENDOR_RUNNER=${5-ubicloud-standard-8} CAPACITY_HOSTED_RUNNER=ubuntu-8core \
    runner_capacity_main 2>&1 >/dev/null
  rm -f "$output"
}
[[ $(paid_line merge_group "$measured") == *"route=hosted"*" paid_runner=ubicloud-standard-8 paid_provider=ubicloud_default" ]]
[[ $(paid_line merge_group "$measured" '' github) == *" paid_runner=ubuntu-8core paid_provider=github" ]]
[[ $(paid_line merge_group "$measured" '' ubicloud) == *" paid_runner=ubicloud-standard-8 paid_provider=ubicloud" ]]
[[ $(paid_line merge_group "$measured" true) != *paid_runner=* ]]
[[ $(paid_line merge_group "$measured" '' '' '') != *paid_runner=* ]]
CAPACITY_ROUTED_EVENT=pull_request
[[ $(paid_line pull_request "$measured") != *paid_runner=* ]]
CAPACITY_ROUTED_EVENT=push

echo 'Runner capacity: owned, retired, unrouted-event, evacuation-switch, retired-routing, census-fallback, routed-event, label and paid-runner controls passed'
