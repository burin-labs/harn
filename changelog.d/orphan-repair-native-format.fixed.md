- **Escalation tool-result pairing is now actually effective on text-primary
  runs (both the declined-dispatch AND the dispatched path).** The #3833 orphan
  repair, and the sibling `record_tool_results` dispatch path, both synthesized
  their tool-result using the session-locked `tool_format`. That lock is pinned
  to the PRIMARY model's format (`text`) at session init and is never re-claimed
  when the run escalates to a native model — so on the exact scenario the repair
  targets (a text-format primary escalating to Anthropic/OpenAI), the tool-result
  took the text-channel branch and was emitted as a bare `role:"user"` message,
  leaving the escalated model's native `tool_use` block orphaned and
  re-triggering the same non-retryable Anthropic HTTP 400. A structured native
  `tool_use`/`tool_call` block is native by definition (text/json channels carry
  calls inline in `content` and produce no structured blocks), so both paths now
  synthesize the tool-result in the provider's native shape (anthropic
  `tool_result`+`tool_use_id`, openai `tool`+`tool_call_id`) when the assistant
  turn carries native blocks, regardless of the session lock. Homogeneous
  text-channel runs and already-dispatched blocks remain strict no-ops.
