- **Structural validator removed.** `std/llm/structural_validator` and
  `with_structural_validator` are gone, along with the agent loop's
  `structural_validator` option, the `structural_validator_decision` agent
  event, and `harn eval coding-agent --structural-validator`. The tool-registry
  readers `tool_required_argument_names`, `tool_required_argument_exemplar`,
  `tool_call_output_schema`, and `exact_tool_call_output_schema` move unchanged
  to `std/llm/tool_call_schema`.

  Migration: stop passing `structural_validator` to `agent_loop`; it is now an
  unknown option, and `agent_loop` refuses the run with `no option reads`.
  Import the four registry readers from `std/llm/tool_call_schema`. Hosts that
  match on the removed `structural_validator_decision` host event kind, or on
  its generated payload type (`HarnAgentEventStructuralValidatorDecision` in
  Swift and Go, the `StructuralValidatorDecision` variant in Rust), must drop
  that case after regenerating their protocol bindings.
