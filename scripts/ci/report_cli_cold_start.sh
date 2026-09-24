#!/usr/bin/env bash
# Project workflow outcomes and the benchmark's own decision into terminal evidence.
set -euo pipefail

build_outcome="${1:-}"
measurement_outcome="${2:-}"
measurement_path="${3:-}"
source_sha="${4:-}"
summary_path="${5:-}"
measurement=null
if [[ -f "$measurement_path" ]]; then
  measurement="$(jq -ce 'select(type == "object")' "$measurement_path")" || measurement=null
fi

evidence="$(jq -n \
  --arg build "$build_outcome" --arg outcome "$measurement_outcome" \
  --arg sha "$source_sha" --argjson m "$measurement" '
  (try (
    $m.schema == "harn.cli_cold_start.measurement.v1"
    and ($sha | test("^[0-9a-f]{40}$")) and $m.source_sha == $sha
    and ($m.expected_commands | type) == "array"
    and all($m.expected_commands[]; type == "string" and length > 0)
    and ($m.expected_commands | unique | length) == ($m.expected_commands | length)
    and ($m.measurements | type) == "object"
    and all($m.measurements[]; (.cold_ms | type) == "number" and .cold_ms >= 0)
    and ($m.failure_reasons | type) == "array"
    and all($m.failure_reasons[]; type == "string" and length > 0)
  ) catch false) as $readable |
  (if $readable then $m.expected_commands - ($m.measurements | keys) else null end) as $pending |
  (if $readable then ($m.measurements | keys) - $m.expected_commands else null end) as $unexpected |
  ($readable and ($m.expected_commands | length) > 0
    and ($pending | length) == 0 and ($unexpected | length) == 0) as $complete |
  (if $build != "success" then "setup_failed"
   elif $complete and $outcome == "failure" and ($m.failure_reasons | length) > 0
     then "budget_failed"
   elif $complete and $outcome == "success" and ($m.failure_reasons | length) == 0
     then "passed"
   else "measurement_failed" end) as $status |
  {
    schema: "harn.cli_cold_start.ci.v1", source_sha: $sha, status: $status,
    build_outcome: $build, measurement_outcome: $outcome, receipt_readable: $readable,
    expected_count: (if $readable then $m.expected_commands | length else null end),
    measured_count: (if $readable then $m.measurements | length else null end),
    pending_count: (if $readable then $pending | length else null end),
    pending_commands: $pending, unexpected_commands: $unexpected,
    failure_reasons: (if $readable then $m.failure_reasons else null end)
  }')"

printf '%s\n' "$evidence"
if [[ -n "$summary_path" ]]; then
  {
    printf '### Cold-start terminal evidence\n\n```json\n'
    printf '%s\n' "$evidence"
    printf '```\n'
  } >> "$summary_path"
fi
[[ "$(jq -r '.status' <<< "$evidence")" == passed ]]
