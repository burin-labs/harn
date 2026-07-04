- **Removed the deprecated `llm_retries` / `llm_backoff_ms` options and the
  in-call transient retry budget.** `llm_call` and `agent_loop` are fail-fast
  on transient provider errors (the `agent_loop` profiles no longer inject
  `llm_retries: 2`); compose retry policy on the caller seam with
  `with_retry(default_llm_caller(), {...})` from `std/llm/handlers` — note the
  off-by-one, `llm_retries: K` → `with_retry(..., {max_attempts: K + 1})`. The
  `deprecated_llm_options` lint is now a hard error carrying that hint. The
  built-in empty-completion retry is a fixed single silent retry for
  provider-shaped routes and can no longer be widened per call.
- **Deleted `std/agent/stack` and the preset wrapper fns.** `agent_stack`,
  `agent_llm_caller`, `agent_tool_stack`, `agent_stack_audit_line`,
  `agent_stack_model_policy`, `agent_budget`, and the eight `*_agent` wrappers
  (`audit_agent` … `release_captain_agent`) are gone; `agent_preset(kind,
  options?)` + `agent_loop` is the single preset surface. The survivors
  `agent_model_options` / `agent_sanitize_model_options` moved to
  `std/agent/options`. See `docs/src/migrations/v0.10.md` for side-by-side
  rewrites.
