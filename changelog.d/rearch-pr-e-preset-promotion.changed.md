- **Promoted `agent_preset` to the stable agent-cell surface** (dropped the
  stale `@api_stability: experimental` tag): `agent_preset(kind, options?)`
  is how you build `agent_loop` options. Kinds now live in a registry —
  `agent_preset_register(kind, {family?, pack?})` makes user-defined kinds
  first-class (validated through the same path as the built-ins) and
  `agent_preset_kinds()` discovers them. Each kind carries fill-nil pack rows
  (per-kind `provider`, `timeout_ms`, session-cumulative `budget`, and
  `model_ladder` defaults) that fill only nil/absent keys and never override
  explicit caller input. Presets also bake a bounded default transport retry
  onto the effective `llm_caller:` seam (`with_retry`, `max_attempts: 3`,
  transport-class failures only — never schema/auth/budget/policy),
  restoring the resilience the removed `llm_retries: 2` profile default used
  to provide; `retry: false` opts out, `retry: {...}` tunes it. Typed alias
  follow-ups: `AgentPresetOptions` gains `retry` / `model_ladder`, and
  `AgentLoopOptions` gains `history` (the #4030 caller-managed history
  seeding option).
