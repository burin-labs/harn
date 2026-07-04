# 2026-07-02 OpenRouter Qwen/GLM Provider Evidence

This note records the source data used to update Harn's default hosted `mid`
preset and the Qwen/GLM OpenRouter catalog rows.

Update 2026-07-04: the `mid` decision below has been superseded by
`2026-07-04-burin-value-route.md`. Qwen 3.6 Flash remains catalogued and useful,
but the hosted `mid` preset now points at
`openrouter:qwen/qwen3-coder-next` after Burin headless probes found better
cost and wall-clock behavior for routine coding-agent tasks.

## Online Sources

- OpenRouter `/api/v1/models`, fetched 2026-07-02, reported:
  - `qwen/qwen3.6-flash`: 1,000,000 context, $0.1875 / $1.125 per MTok,
    `tools`, `tool_choice`, `response_format`, `structured_outputs`, and
    `reasoning`.
  - `qwen/qwen3.6-plus`: 1,000,000 context, $0.325 / $1.95 per MTok, same
    tool/structured/reasoning parameters.
  - `qwen/qwen3.6-35b-a3b`: 262,144 context, $0.14 / $1.00 per MTok, same
    tool/structured/reasoning parameters.
  - `z-ai/glm-5.2`: 1,048,576 context, $0.93 / $3.00 per MTok, `tools`,
    `tool_choice`, `response_format`, `structured_outputs`, `reasoning`, and
    `reasoning_effort`.
- OpenRouter's Qwen model page lists `qwen/qwen3.6-flash` with a 1M context
  window, $0.1875 / $1.125 pricing, prompt caching, and multimodal input:
  <https://openrouter.ai/qwen>.
- OpenRouter's Z.ai page describes GLM 5.2 as a 1M-context long-horizon agent
  model and lists $0.93 / $3.00 pricing:
  <https://openrouter.ai/z-ai>.
- Z.ai's official GLM-5.2 docs describe the model as a 1M-context
  long-horizon coding-agent model, and the upstream `zai-org/GLM-5` README
  repeats the 1M context plus flexible thinking-effort claims:
  <https://docs.z.ai/guides/llm/glm-5.2> and
  <https://github.com/zai-org/GLM-5>.
- Together's GLM-5.2 docs are a useful independent provider cross-check for
  GLM-specific request handling: they document function calling, structured
  outputs, adjustable `reasoning_effort`, and thinking enabled by default:
  <https://docs.together.ai/docs/glm-5.2-quickstart>.
- OpenRouter's model and parameter docs define `supported_parameters`,
  including `tools`, `tool_choice`, `response_format`, `structured_outputs`,
  `reasoning`, `include_reasoning`, and `reasoning_effort`:
  <https://openrouter.ai/docs/guides/overview/models> and
  <https://openrouter.ai/docs/api/reference/parameters>.
- Public tool-call caution remains warranted for GLM-family routes:
  OpenAgentsInc/openagents#6310 reports GLM 5.2 tool-call failures in a coding
  agent, and anomalyco/opencode#32172 tracks GLM 5.2 support with explicit
  tool-call parsing caveats:
  <https://github.com/OpenAgentsInc/openagents/issues/6310> and
  <https://github.com/anomalyco/opencode/issues/32172>.
- Additional public GLM-5.2 caution has appeared since the first probe:
  QwenLM/qwen-code#6007 reports GLM-5.2 reasoning text leaking into normal
  assistant output when a 128K `max_tokens` cap is sent, while coder/coder#26469
  reports an OpenRouter GLM-5.2 stream parser failure through an AI gateway.
  These reports line up with Harn's own conservative `native_unreliable` /
  text-tool caution and argue against making GLM 5.2 the value default until
  longer agent-loop probes demonstrate stable tool and reasoning-channel
  behavior:
  <https://github.com/QwenLM/qwen-code/issues/6007> and
  <https://github.com/coder/coder/issues/26469>.
- microsoft/copilot-intellij-feedback#1874 reports OpenRouter/Fireworks tool
  calls failing with provider HTTP 400 when assistant history re-sends
  nonstandard nested `tool_calls[]` fields such as `approxNumTokens`. Harn's
  OpenAI-compatible boundary strips those nested fields and normalizes Harn's
  internal flat `{name, arguments}` tool-call records back to strict
  OpenAI-compatible `{type: "function", function: ...}` history before dispatch:
  <https://github.com/microsoft/copilot-intellij-feedback/issues/1874>.
- OpenRouter `/api/v1/models`, fetched 2026-07-03, additionally reports cache
  pricing fields that were absent from the first source patch:
  `qwen/qwen3.6-flash.pricing.input_cache_write = "0.000000234375"` and
  `z-ai/glm-5.2.pricing.input_cache_read = "0.00000018"`.

## Live Probe

Command shape: direct OpenRouter chat completions, temperature 0, usage
included, Harn-style reasoning suppression (`reasoning.enabled=false`) for the
structured JSON, native-tool, and text-tool smoke tasks.

| Model | Structured JSON | Native Tool | Harn Text Tool | Total Probe Cost |
| --- | ---: | ---: | ---: | ---: |
| `qwen/qwen3.6-flash` | 1.23s, pass | 0.66s, pass | 0.83s, pass | $0.000302 |
| `qwen/qwen3.6-plus` | 3.29s, pass | 1.76s, pass | 0.90s, pass | $0.000498 |
| `qwen/qwen3.6-35b-a3b` | 2.31s, pass | 7.14s, pass | 0.31s, pass | $0.000246 |
| `z-ai/glm-5.2` | 2.30s, pass | 0.50s, pass | 0.40s, malformed close | $0.001258 |

Decision at the time: use Qwen 3.6 Flash as the hosted `mid` preset. This is
now superseded by the 2026-07-04 Burin value-route probe. Keep GLM 5.2
catalogued and effort-aware, but do not promote it to the default until longer
agent-loop probes overcome the existing GLM tool-call, streaming, and
reasoning-channel reliability evidence.

Follow-up sanity probe, 2026-07-03: direct OpenRouter chat completions with
temperature 0 and `reasoning.enabled=false` still passed the tiny structured
JSON and forced-native-tool checks for both `qwen/qwen3.6-flash` and
`z-ai/glm-5.2`.

| Model | Structured JSON | Forced native tool | Approx cost |
| --- | ---: | ---: | ---: |
| `qwen/qwen3.6-flash` | 0.72s, pass | 0.96s, pass | $0.000134 |
| `z-ai/glm-5.2` | 2.76s, pass | 0.69s, pass | $0.000357 |

This probe is only a transport/shape sanity check. It supports keeping Qwen as
the cheap/value default, while GLM remains a worthwhile escalation candidate for
longer coding-agent evals where its higher output price can earn its keep.

Product guidance: Harn's provider catalog and presets are the single source of
truth for this decision. Host products such as Burin should display or select
these facts through the generated catalog and should not carry parallel
provider-specific request-shape, pricing, tool-format, or reasoning-effort
quirks.
