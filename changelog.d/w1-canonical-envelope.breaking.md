`llm_call` now returns one canonical response envelope. `usage` is the single
owner of all accounting — the top-level duplicates (`input_tokens`,
`output_tokens`, `cache_read_tokens`, `cache_write_tokens`,
`cache_creation_input_tokens`, `served_fast`, `cache_hit_ratio`,
`cache_visibility`, `cache_savings_usd`, `provider_telemetry`) and the alias
keys (`prose`, `private_reasoning`, `parsed_done_marker`,
`usage.cache_creation_input_tokens`) are removed; `tool_calls` is always
present. Every response now carries a typed `outcome: {kind, billed}`
(`complete` | `tool_use` | `truncated` | `refused` | `paused` | `empty`), and
streaming chunks use `stop_reason` (was `finish_reason`). New `std/llm/envelope`
module exports the typed contract (`LlmResponse`, `LlmUsage`, `LlmOutcome`, …);
`std/llm/handlers` gains `llm_caller()` (the blessed default caller with typed
retry statuses and billed-empty re-dispatch), and `safe_call` /
`default_llm_caller` failures now map onto the reserved status vocabulary
(including the new never-retried `provider_error`) instead of only
`budget_exhausted` / `exception`. Session-usage records and receipts no longer
emit the `cache_creation_input_tokens` alias (ingest still accepts it).
