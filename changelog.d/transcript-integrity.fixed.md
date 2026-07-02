- **Agent-loop transcript integrity: no more orphaned tool_use / tool_result
  pairs.** Skip paths that persist an assistant tool_use turn without
  dispatching (pre-dispatch user interrupt, `agent_await_resumption`
  suspension and its parallel siblings, invalid await arguments) now record
  synthesized placeholder tool_results (`interrupted` /
  `awaiting_resumption` / `skipped`), so Anthropic-native sessions no longer
  400 with "tool_use ids were found without tool_result blocks" after an
  interrupt or resume. The Anthropic egress normalizer additionally
  backfills a deterministic placeholder tool_result for any orphaned
  tool_use id as a safety net, and auto-compaction never splits the kept
  window between an assistant tool-use message and its tool_result
  message(s).
