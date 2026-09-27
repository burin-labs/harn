An unavailable step judge is now loud. Its `step_judge_decision` reports `verdict: "unavailable"`,
never `pass`, with a typed `unavailable_reason` and a running `unavailable_count`. A label-only
decision model configured as a judge is refused before dispatch instead of failing on every step.
A `replace` veto on a turn that parse repair already answered no longer crashes the loop: it
applies as `retain`. A purpose-label block next to a valid call is no longer reported as an
unparsed tool call.
