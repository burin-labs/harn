- **Escalation no longer orphans a `tool_use` block into an Anthropic HTTP
  400.** When a text-format primary model escalated to a native-format model
  (e.g. Fireworks → Anthropic), the escalated model would emit a real
  `tool_use` block that the loop then declined to dispatch (native-format
  fallback reject, all-blank-name drop, parse-error, or no-progress nudge) and
  followed with a bare user-feedback message. That left the assistant
  `tool_use` with no matching `tool_result`, which Anthropic rejects with a
  non-retryable HTTP 400 (`tool_use ids were found without tool_result blocks
  immediately after`), killing the run before the escalated fix was applied.
  Every such inject path now first synthesizes a matching `tool_result` for
  each orphaned block (carrying the same corrective feedback as its
  observation) via the shared `agent_session_pair_orphaned_tool_use` repair, so
  the pairing invariant holds across the native / OpenAI / Gemini wire shapes.
  The repair is a strict no-op for homogeneous text-format runs (whose calls
  stay inline in `content`) and for blocks the loop already dispatched, so
  converging runs are unaffected.
