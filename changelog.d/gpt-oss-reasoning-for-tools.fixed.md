- **gpt-oss (Harmony) now keeps reasoning ON for tool calls — kills the
  billed-noncommittal failure at its root.** gpt-oss performs tool calls
  *inside* the Harmony chain-of-thought channel, so disabling reasoning breaks
  tool calling entirely (live OpenRouter probe of `openai/gpt-oss-120b`:
  `reasoning {enabled:false}` → 0 tool_calls + null completion_tokens; `effort:
  low` / provider default → clean native tool calls). This is the *opposite* of
  the Qwen3 quirk (Qwen narrates tool intent in the reasoning trace and emits
  zero `tool_calls`, so Qwen needs reasoning OFF for tools), and #3303's retry
  was masking this self-inflicted misconfig. The fix is declarative, in the
  `capabilities.toml` family: a new `reasoning_required_for_tools` capability
  flag is set on every gpt-oss row (Together, Groq, Cerebras, and a newly-added
  OpenRouter `openai/gpt-oss-*` row that previously fell through to a
  reasoning-less catch-all), and `reasoning_policy` now refuses to resolve a
  tool-bearing task (agent/code/verify) to reasoning-off when that flag is set —
  flooring to the lowest supported effort instead — so no future auto default,
  capability override, or session pin can re-introduce the failure. The Qwen3
  reasoning-off-for-tools behavior is unchanged. Both quirks are now documented
  side by side in the capability matrix.
