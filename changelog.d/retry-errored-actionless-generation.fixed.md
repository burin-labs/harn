- **Errored / tool-call-less generations are retried instead of advancing the
  loop on a broken turn.** A cheap model sometimes returns a generation that
  ends with a provider error (`stop_reason == "error"`) after only *narrating*
  an intended tool call in its text/reasoning (e.g. "We need to make edit to
  create tests/...test.cpp...") while emitting ZERO parsed tool calls. The agent
  loop used to advance on that turn and reply with a generic
  `no_progress`/`stall_diagnostics` nag that never told the model its turn
  errored, so after a few such turns the model gave up having written nothing.
  This is distinct from the zero-token empty-completion retry (those have
  non-zero reasoning tokens, so the zero-token predicate misses them).
  `observed_llm_call` now treats an errored-but-actionless `Ok` generation as a
  transient provider hiccup and retries it within the same bounded
  empty-completion budget (no change to the global retry default). When the
  budget is exhausted and the loop does advance, the stall detector emits
  cause-specific feedback ("your previous turn ended with a provider error and
  emitted no tool call — re-emit the intended tool call") instead of the generic
  no-progress nag, while genuine no-tool-intent stalls keep the existing nag.
