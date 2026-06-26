- `std/llm/safe`: the structured-output **repair** side-call now floors its
  output budget reasoning-awarely (`repair_min_max_tokens`), the twin fix to the
  #3598 judge floor. A flat 600-token repair budget was billed against the same
  `max_tokens` as a reasoning route's hidden analysis channel, truncating the
  repaired JSON verdict to empty (a silent dead-repair) on reasoning models such
  as gpt-oss. Non-reasoning routes are unchanged (600 baseline); reasoning routes
  are raised to `reasoning_budget + verdict headroom`. Applied in
  `safe_structured_call`'s judge defaults and the `with_repair` caller-seam
  handler, reusing the same floor as the judge path so the two never drift.
