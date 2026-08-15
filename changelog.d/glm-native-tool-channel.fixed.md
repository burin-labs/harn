- **GLM routes use their provider-native tool channel again, and the latest
  DeepSeek V4 snapshots are catalogued.** The `glm-5` cross-host
  `native_unreliable` verdict claimed the weights leak `<tool_call>` markup into
  assistant content on every host. A re-probe across zai-direct, OpenRouter,
  Fireworks, NVIDIA, Together and Cerebras found zero markup leaks in 19/19
  checks, so GLM now prefers `native` instead of paying for a text-grammar hop.
  DeepInfra's GLM-5.2 route keeps a fenced-JSON pin for a real, host-specific
  reason: under `tool_choice = "required"` it returns 38 duplicate tool calls
  for one intent. Adds `deepseek/deepseek-v4-pro-0813`,
  `deepseek/deepseek-v4-flash-0731` and `deepseek-ai/DeepSeek-V4-Flash-0731`,
  and corrects the OpenRouter DeepSeek V4 rate card, which carried DeepSeek's
  direct prices and understated V4 Pro input by ~2.7x.
