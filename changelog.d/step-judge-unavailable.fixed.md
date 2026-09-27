An unavailable step judge is now loud. Its `step_judge_decision` reports `verdict: "unavailable"`,
never `pass`, with a typed `unavailable_reason` and a running `unavailable_count`. A label-only
decision model configured as the judge is refused before dispatch instead of failing on every step.
