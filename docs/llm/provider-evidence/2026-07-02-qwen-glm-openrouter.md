# 2026-07-02 OpenRouter Qwen/GLM Provider Evidence

This note records the source data used to update Harn's default hosted `mid`
preset and the Qwen/GLM OpenRouter catalog rows.

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
- OpenRouter's Qwen 3.6 Plus page advertises a 1M context window, $0.325 /
  $1.95 pricing, and 78.8 SWE-bench Verified:
  <https://openrouter.ai/qwen/qwen3.6-plus>.
- OpenRouter's Z.ai page describes GLM 5.2 as a 1M-context long-horizon agent
  model and lists $0.93 / $3.00 pricing:
  <https://openrouter.ai/z-ai>.
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

Decision: use Qwen 3.6 Flash as the hosted `mid` preset. It matched Plus on
the smoke matrix, was faster in these probes, keeps a 1M context window, and is
cheaper than Plus or GLM. Keep GLM 5.2 catalogued and effort-aware, but do not
promote it to the default until longer agent-loop probes overcome the existing
GLM tool-call reliability evidence.
