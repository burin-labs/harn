- **Agent loop now emits a final wrap-up turn on budget/iteration exhaustion.**
  When the loop terminated because it ran out of iterations or budget *while the
  model was still calling tools*, the surfaced final assistant text was whatever
  the last tool-call turn produced — a dangling tool call with no clean
  `<user_response>` or completion sentinel, so output-contract / done-sentinel
  checks failed even when the work succeeded. The loop now fires exactly one
  tool-less LLM call on exhaustion/cap (`budget_exhausted` / `verify_capped` /
  `verify_exhausted` / `stuck`) to elicit the model's final answer + sentinel and
  records it as the final assistant response, so the run ends with a real summary
  instead of a dangling tool call. The wrap-up never changes
  `final_status`/`stop_reason`, is skipped on clean completion / suspension /
  terminal errors, and is opt-out via `final_wrapup: false`.
