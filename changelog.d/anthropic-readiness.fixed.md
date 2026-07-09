- Fixed provider readiness probes so Anthropic uses its catalogued `/models`
  endpoint, native providers without a model inventory endpoint fail explicitly,
  and model matching no longer accepts arbitrary string prefixes.
- Kept private reasoning/thinking content blocks out of provider and ACP replay
  request bodies while preserving internal tool calls and tool results for agent
  turns.
