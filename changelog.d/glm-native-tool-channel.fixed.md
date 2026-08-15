- **GLM routes use their provider-native tool channel again, six unreachable
  routes are retired, and the latest DeepSeek V4 snapshots are catalogued.** The
  `glm-5` cross-host `native_unreliable` verdict claimed the weights leak
  `<tool_call>` markup into assistant content on every host. A re-probe across
  zai-direct, OpenRouter, Fireworks, NVIDIA, Together and Cerebras found zero
  markup leaks in 19/19 checks, so GLM now prefers `native` instead of paying
  for a text-grammar hop. DeepInfra's GLM-5.2 route keeps a fenced-JSON pin for
  a real, host-specific reason: under `tool_choice = "required"` it returns 38
  duplicate tool calls for one intent.

  A reachability sweep of all 34 catalogued GLM/DeepSeek routes then found 6
  that no provider serves any more, each removed or repointed at the build the
  host actually runs: NVIDIA end-of-lifed `z-ai/glm-5.1` and both undated
  DeepSeek V4 builds, Together serves GLM-5.1 and GLM-5 only through a
  provisioned dedicated endpoint, and Fireworks no longer deploys `glm-5p1`.

  Adds `deepseek/deepseek-v4-pro-0813`, `deepseek/deepseek-v4-flash-0731`,
  `deepseek-ai/DeepSeek-V4-Flash-0731` and `nvidia/deepseek-v4-flash-0731`, and
  corrects the OpenRouter DeepSeek V4 rate card, which carried DeepSeek's direct
  prices and understated V4 Pro input by ~2.7x.
