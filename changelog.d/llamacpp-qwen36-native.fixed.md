- **llama.cpp Qwen3.6 routes default to native tool calls again (harn#5162).** The `*qwen3.6*` llama.cpp
  capability rule is re-promoted to `native_tools = true` / `preferred_tool_format = "native"` with
  `tool_mode_parity = "text_unreliable"`, matching the mlx and local-vLLM siblings that serve the same weights and
  chat template. 2026-07-18 evidence with `--jinja` + the model's tool template: 5/5 forced-native trials on a
  13-action edit schema selected the correct action with all required fields, while the json text channel produced
  zero parseable write calls in a live agent session. The 2026-07-17 demotion predated confirmation that the serving
  config applied the tool template.
