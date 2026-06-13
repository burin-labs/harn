- **Gemini thinking-budget quirks moved from hard-coded Rust branches into the
  `capabilities.toml` declarative matrix.** `gemini.rs` previously decided
  whether a Gemini model supported a thinking budget, whether thinking could be
  disabled, and what the high/xhigh budget ceiling was via inline
  `model.contains("gemini-2.5")` / `model.contains("flash")` branches. Those
  facts are now declared alongside each model's other wire capabilities: a new
  `max_thinking_budget` capability field (Gemini 2.5 Flash 24576, Pro 32768)
  plus the existing `reasoning_disable_supported` (Flash can disable thinking,
  Pro cannot) and `thinking_modes` (effort support gates thinkingConfig). The
  provider now reads `capabilities::lookup("gemini", model)` and the
  per-model patterns live in the matrix, matching the
  `auto_reasoning_overrides` precedent. Behavior is identical (the unreachable
  speculative `robotics` branch — no such catalogued model exists — is the only
  dropped path, folded into the declared per-row flags). Verified by the
  existing `gemini_thinking_config_maps_from_typed_thinking` golden test.
