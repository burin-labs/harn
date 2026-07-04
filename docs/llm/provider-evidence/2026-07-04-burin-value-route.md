# 2026-07-04 Burin/Harn Value Route Evidence

This note records the source and probe evidence behind the hosted `mid` preset
moving to `openrouter:qwen/qwen3-coder-next`.

## Online Sources

- OpenRouter's Qwen3-Coder-Next page lists it as an open-weight coding-agent
  model with 80B total / 3B active parameters, 262K context, non-thinking mode,
  and $0.11 / $0.80 per MTok pricing:
  <https://openrouter.ai/qwen/qwen3-coder-next>.
- OpenRouter `/api/v1/models`, fetched 2026-07-04, reported
  `qwen/qwen3-coder-next` cache-read pricing at $0.07/MTok and confirmed
  `tools`, `tool_choice`, `response_format`, and `structured_outputs`.
- Z.AI's API reference and OpenAI SDK docs list the general OpenAI-compatible
  base URL as `https://api.z.ai/api/paas/v4`:
  <https://docs.z.ai/api-reference/introduction> and
  <https://docs.z.ai/guides/develop/openai/python>.
- Z.AI's Coding Plan integration guide lists the OpenAI-compatible coding-plan
  endpoint as `https://api.z.ai/api/coding/paas/v4`:
  <https://docs.z.ai/devpack/tool/others>.
- OpenRouter `/api/v1/models`, fetched 2026-07-04, reported
  `z-ai/glm-5.2` at $0.91 / $2.86 per MTok, $0.169/MTok cache-read, 1M context,
  and native `reasoning_effort`.
- vLLM issue #39056 documents a Qwen-family parser failure mode where tool
  calls inside reasoning can be lost before reaching the tool parser. Harn's
  default route therefore prefers the non-thinking Qwen3-Coder-Next model and
  keeps provider/tool-format rules centralized in the catalog:
  <https://github.com/vllm-project/vllm/issues/39056>.

## Burin Headless Probes

Read-only tiny repo summary:

| Route | Provider/model | Tool format | TTFT | Wall | Cost | Result |
| --- | --- | --- | ---: | ---: | ---: | --- |
| `balanced` | OpenRouter `qwen/qwen3.6-flash` | native | 6.06s | 10.87s | $0.0090 | pass |
| `qwen3-coder-next` | OpenRouter `qwen/qwen3-coder-next` | text | 5.81s | 8.50s | $0.0034 | pass |
| `qwen3.7-plus` | OpenRouter `qwen/qwen3.7-plus` | native | 9.88s | 25.31s | $0.0176 | pass, low confidence |
| `openrouter-glm-5.2` | OpenRouter `z-ai/glm-5.2` | text | 4.34s | 6.61s | $0.0287 | pass |
| `glm-5.2` | Z.AI direct | text | n/a | n/a | n/a | provider 404 before endpoint fix |
| `or-kat-coder-pro-v2` | OpenRouter KAT | native | 5.34s | 12.35s | $0.0179 | pass |

Tiny edit-and-test probe:

| Route | Outcome | Wall | Cost | Notes |
| --- | --- | ---: | ---: | --- |
| `balanced` | fixed and verified | 132s | $0.244 | repeated green test loop |
| `qwen3-coder-next` | fixed and verified | 22s | $0.016 | best value |
| `qwen3.7-plus` | file fixed, timed out | 240s | $0.263 | repeated unnecessary work |
| `openrouter-glm-5.2` | fixed | 197s | $1.54 | expensive loop/churn |
| `glm-5.2` | provider failure | n/a | n/a | old direct endpoint |
| `or-kat-coder-pro-v2` | file fixed, budget exhausted | n/a | ~$0.54 | repeated tests |

## Harn Fixes

- Direct Z.AI now defaults to `https://api.z.ai/api/paas/v4`; users with GLM
  Coding Plan quota can set `ZAI_BASE_URL=https://api.z.ai/api/coding/paas/v4`.
- The hosted `mid` and `tier/mid` aliases now resolve to
  `openrouter:qwen/qwen3-coder-next`.
- OpenRouter GLM-5.2 and Qwen3-Coder-Next pricing/cache fields were refreshed
  from the live OpenRouter catalog.
- The agent loop now distinguishes "write succeeded" from "latest write was
  verified green" and stops instead of extending budget on repeated identical
  passing checks.
- Internal completion/judge verdict JSON is stripped before assistant turns are
  recorded as visible text.
- Nested execution descent emits debug metadata instead of default stderr info.

Decision: Qwen3-Coder-Next is the best hosted value default for routine Burin
coding-agent work in this probe set. GLM-5.2 remains a strong long-context
frontier/escalation candidate, but its higher cost and observed churn do not
justify making it the default value route yet.
