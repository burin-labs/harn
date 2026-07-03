- **Agent-run traces now carry first-class per-LLM-call token and cost
  attribution, plus tool-selection span kinds.** Each `llm_call` span records
  structured token usage keyed by canonical metadata constants
  (`input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_write_tokens`,
  `model`, `provider`) built through the typed `LlmCallUsage` helper, and its
  `cost_usd` is now `None` (honest "unpriced") rather than a misleading `0.0`
  when the (provider, model) pair has no catalog entry. `RunTraceSpanRecord`
  gains an optional, first-class `cost_usd` field so downstream viewers (Burin
  portal, harn-cloud dashboard) can build token/cost flame graphs without
  reconstructing them from cumulative-usage diffs; the field defaults to `None`
  so records persisted before it existed still load. Three point-in-time marker
  span kinds are added for the tool surface — `model_route`
  (`from_model`/`to_model`/`reason`), `tool_mount`
  (`tool_names`/`tool_count`/`source`/`detail`), and `deferred_tool_load`
  (`tool_name`/`query`/`score`) — emitted via typed `emit_*` helpers. `tool_mount`
  is wired at MCP bootstrap; `model_route` and `deferred_tool_load` ship the
  emission API for the escalation and `tool_search`-promotion sites to call.
  Telemetry only — no agent behavior changes.
