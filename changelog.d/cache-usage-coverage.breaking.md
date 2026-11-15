- **Run and session usage retain measured cache coverage.** Public usage views
  include the canonical cache-category report counts and an optional physical
  provider-call count, so clients can distinguish complete totals, partial
  subtotals, measured zero, and legacy unknown coverage. The JSON fields remain
  optional for older records.

  Migration: Rust consumers constructing `LlmUsageRecord`, `RunViewUsage`, or
  `LlmTraceUsageSummary` with exhaustive literals must supply the new optional
  fields or use `..Default::default()`. Exhaustive patterns must include the new
  fields or `..`. Treat absent coverage as unknown rather than zero, and compare
  category report counts with physical `provider_call_count`, not logical
  `call_count`.
