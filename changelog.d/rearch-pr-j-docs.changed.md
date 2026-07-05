- **Documentation: consolidated the R1 orchestration-primitive guidance.**
  [Choosing an agent abstraction](docs/src/concepts/abstraction-ladder.md) now
  opens with the one-line ladder — `llm_call` (one request) < `agent_loop` (one
  goal, run to completion) < `workflow` (more than one goal, attempt, or model) —
  spells out the "never hand-write a `while` around `llm_call`" rule, and states
  explicitly that `agent_preset` and model ladders are *not* rungs. It adds a
  **placement contract** table naming the canonical home for every cross-cutting
  mechanism (completion gate → `std/agent/judge`, governors → `std/agent/governors`,
  unified detectors → `std/agent/stall`, lanes → `std/agent/lanes`, overlays →
  `std/agent/overlays`, compaction → `std/agent/autocompact`, scratchpad →
  `std/agent/scratchpad`, default mutation tools → `agent_edit_tools`, and preset
  packs → `std/agent/presets`). The `harn-orchestration` skill gained the same
  ladder framing plus the `models:` / `ladder:`, `agent_edit_tools`,
  retry-with-feedback (`repair_prompt_builder` / `feedback`), fn-verify,
  `workflow_stages`, and `workflow_run_repair` surfaces. The `llm_call` options
  reference now documents `models:` / `ladder:` and points to the 0.10 migration
  for the removed `llm_retries` / `llm_backoff_ms` / `transcript_policy` options.
