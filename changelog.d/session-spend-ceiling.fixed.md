- **An ACP session's LLM cost ceiling now covers the whole session.** Each `session/prompt` used to start its
  cost scope at $0, so a session past its cap was admitted again on the next prompt and after every resume.
  The session carries its spend across prompts, persists it as the row's `usage_cost_usd_micros`
  (which `session/list` reports), and seeds it on `session/load`; older rows are backfilled from their recorded
  `llm_call` costs. A `session/set_budget` re-arm now also applies to later turns of that session.
