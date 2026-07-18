- Add first-class Vercel AI Gateway catalog routes, aliases, Responses API support, creator/model capability
  inheritance, generic routing metadata telemetry, and documented provider routing controls.
- Preserve gateway `provider_metadata` from both regular responses and final streaming frames so receipts can audit
  resolved upstreams, fallback attempts, and exact billed cost.
- Resolve provider wire ids back to collision-free catalog routes for pricing, and let the coding-agent evaluator
  exercise the runtime's fenced-JSON tool grammar alongside native and tagged-text formats.
- Normalize fenced-JSON calls into the same recorded tool results as native and tagged-text calls, prefer dispatched
  tools over premature same-turn completion markers, and collapse only exact duplicate read-only text-channel calls.
