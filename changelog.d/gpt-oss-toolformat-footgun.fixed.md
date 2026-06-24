- **gpt-oss (Harmony) and GLM-5.x native tool-call footguns are now closed at
  the capability layer.** DeepInfra and SambaNova `gpt-oss` and the zai-direct
  `glm-5.x` routes are pinned to the TEXT tool channel with
  `tool_mode_parity = "native_unreliable"`, so a `native` pin (alias or
  `--tool-format native`) auto-corrects to `text` with an explanatory
  `correction` instead of silently emitting an empty tool stream. The
  provider-native Harmony channel on these pay-per-token routes drops tool calls
  into the private reasoning/commentary channel (empty `tool_calls` /
  billed-noncommittal), matching the Fireworks `#3505` precedent and the same
  failure class reported on vLLM, SGLang, and the OpenAI Harmony repo.
- **New first-class "no viable tool channel" fail-fast guard
  (`capabilities::no_viable_tool_channel`).** When a `(provider, model)` route
  has neither a trusted native nor a trusted text tool channel, a tool-bearing
  `llm_call` now fails before dispatch with an actionable error naming the bad
  combo and a suggested alternative, instead of billing a noncommittal completion
  with no dispatchable tool call.
