- **Cross-provider escalation from an OpenAI/Ollama-dialect primary to
  Anthropic no longer dies with `messages: Unexpected role "tool"` (HTTP 400).**
  A cheap OpenAI-dialect primary (e.g. Fireworks gpt-oss escalating to Claude
  Sonnet) records tool results as top-level `role:"tool"` messages. When
  escalation switched the provider to Anthropic and replayed that history,
  Anthropic rejected `role:"tool"` — it represents a tool result as a
  `role:"user"` message carrying a `tool_result` content block keyed by
  `tool_use_id`, never a top-level `role:"tool"`. The Anthropic request builder
  now translates any `role:"tool"` message into that shape at the egress
  boundary (before the canonical-key retain that would otherwise strip the
  source `tool_call_id`, and before tool-result adjacency enforcement so the
  real observation pairs with its `tool_use` block instead of being masked by an
  interrupted-before-dispatch placeholder). The quirk lives in the Anthropic
  adapter, so homogeneous-Anthropic and homogeneous-OpenAI/Ollama runs are
  byte-identical — only the cross-dialect escalation case changes.
