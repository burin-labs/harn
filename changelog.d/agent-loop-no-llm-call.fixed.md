- **Fail loud on a model-less agent turn.** When `agent_loop` finalizes a
  completed turn that never actually called the provider (zero iterations and
  zero tokens for a `done`/empty status), it now returns a clear `no_llm_call`
  terminal error — "agent turn made no LLM call: no model resolved / empty
  input" — instead of a silent success-with-empty-text. Intentional pauses
  (`suspended`, `blocked`, `cancelled`, waitpoints) and already-errored turns
  are unaffected.
