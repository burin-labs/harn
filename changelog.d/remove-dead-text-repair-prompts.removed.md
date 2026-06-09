- `std/agent/prompts`: removed the unused `action_required_feedback`,
  `action_turn_nudge`, and `protocol_violation_feedback` prompt entrypoints
  (their `*_prompt` functions, registry/catalog/override entries, and
  `.harn.prompt` exemplar files). They had no in-tree caller — the live tool-
  call repair path uses the parametric `parse_guidance` prompt — and the
  `protocol_violation_feedback` exemplar hardcoded a text-format `<tool_call>
  name({...})` shape that does not apply to json/native tool-format sessions.
