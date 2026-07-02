# 2026-07-02 OpenRouter Qwen 3.7 Provider Evidence

This note records the evidence behind Harn's Qwen 3.7 OpenRouter catalog
metadata and why the hosted `mid` preset stays on Qwen 3.6 Flash.

## Online Sources

- OpenRouter `/api/v1/models`, fetched 2026-07-02, reported:
  - `qwen/qwen3.7-plus`: 1,000,000 context, $0.32 / $1.28 per MTok,
    cache read/write pricing, `tools`, `tool_choice`, `response_format`,
    `structured_outputs`, and `reasoning`.
  - `qwen/qwen3.7-max`: 1,000,000 context, $1.25 / $3.75 per MTok,
    cache read/write pricing, `tools`, `tool_choice`, `response_format`,
    `structured_outputs`, and `reasoning`.
- OpenRouter's Qwen 3.7 Plus page describes Plus as cost-effective and lists
  $0.32 / $1.28 pricing with a 1M context window:
  <https://openrouter.ai/qwen/qwen3.7-plus>.
- OpenRouter's Qwen 3.7 Max page describes Max as the flagship Qwen 3.7 model
  for agent-centric workloads and lists $1.25 / $3.75 pricing with a 1M context
  window:
  <https://openrouter.ai/qwen/qwen3.7-max>.
- Qwen's launch note describes Qwen 3.7 Max as an agent-capability focused
  model:
  <https://qwen.ai/blog?id=qwen3.7>.

## Live Probe

Artifact: `/tmp/harn-openrouter-qwen37-probe-20260702T183233Z.json`.

Command shape: direct OpenRouter chat completions, temperature 0, usage
included, Harn-style reasoning suppression (`reasoning.enabled=false`) for
structured JSON, native-tool, and Harn text-tool smoke tasks.

| Model | Structured JSON | Native Tool | Harn Text Tool | Total Wall | Total Cost |
| --- | --- | --- | --- | ---: | ---: |
| `qwen/qwen3.6-flash` | schema miss | pass, 0.73s | JSON-shaped text call | 2.51s | $0.000158 |
| `qwen/qwen3.7-plus` | schema miss | pass, 1.26s | JSON-shaped text call | 6.06s | $0.000324 |
| `qwen/qwen3.7-max` | schema miss | pass, 1.52s | JSON-shaped text call | 5.62s | $0.001016 |

Decision: keep Qwen 3.6 Flash as the hosted `mid` value preset. Qwen 3.7
Plus/Max are catalogued as current Qwen routes and pass native tool dispatch,
but this probe did not show a latency or cost reason to replace Flash for the
default value route. Max is the flagship/high-price route, so it belongs in the
`frontier` tier rather than the `mid` tier.
