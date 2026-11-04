- **Structural validator removed.** `std/llm/structural_validator` and
  `with_structural_validator` are gone, along with the agent loop's
  `structural_validator` option, the `structural_validator_decision` agent
  event, and `harn eval coding-agent --structural-validator`. The tool-registry
  readers `tool_required_argument_names`, `tool_required_argument_exemplar`,
  `tool_call_output_schema`, and `exact_tool_call_output_schema` move unchanged
  to `std/llm/tool_call_schema`.

  Migration: stop passing `structural_validator` to `agent_loop`; the loop no
  longer reads it, and passing it has no effect. Import the four registry readers from `std/llm/tool_call_schema`.
  Hosts that match on the `structural_validator_decision` event kind or the
  generated `StructuralValidatorDecision` type must drop that case after
  regenerating their protocol bindings.
