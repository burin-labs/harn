- **Input guardrail and pre-turn scope classifier removed.** `agent_loop` no
  longer accepts `input_guardrail` or `pre_turn_scope_classifier`, and the
  `status: "input_guardrail"` / `status: "scope_alert"` terminals,
  `std/agent/guardrails` (`agent_input_guardrail`,
  `agent_input_guardrail_check`), `std/llm/scope_classifier`
  (`pre_turn_scope_classifier`), `lane_scope_classifier`, `harn eval
  scope_triage`, the `HARN_EVAL_SCOPE_TRIAGE_*` variables, and the
  `input_guardrail_verdict` / `scope_classifier_verdict` agent events are gone.
  Both hooks were opt-in and off by default. Neither was a security boundary:
  each was a model judgment that failed open on classifier error, and the
  sandbox and tool policy do the enforcing. To stop a request before the first
  model turn, check it in the caller before starting `agent_loop`, or deny it
  through tool policy.

  Migration: delete `input_guardrail` and `pre_turn_scope_classifier` from
  `agent_loop` options (an unknown key is now ignored at run time and rejected
  by `harn check` on typed options), and drop handling of the two event kinds
  and the two terminal statuses. Regenerate protocol bindings.
