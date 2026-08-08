# 2026-07-28 provider tool-call scorecard

This campaign followed up #4192 with the spend-capped
`provider_tool_probe_campaign.harn` runner. Each complete route used five live
non-streaming attempts for seven fixed cases in each of the native, fenced-JSON,
and tagged-text formats: 105 physical requests per route. The campaign stopped
on any response without runtime-priced usage rather than continuing against an
unknown ledger.

The run used Harn runtime revision
`e7898bd182a38422f9e012c63c8cd4c61d254932`, provider-catalog hash
`sha256:96b7a047ee4abc838aec24d58957eeca0c9b083c58179d376f27716c8e8ad9e2`,
and exact campaign-source hash
`sha256:433bda58c6e6d3752541bdad991facdf77fb4ecc638392bc8389fa71f33f9083`.
That hash identifies the bytes used for the paid run. The final checked-in
source was passed through `harn fmt` afterward and conservatively marks a child
that produces no parseable receipt as one unpriced attempt, stopping subsequent
calls. Every child in the paid run produced a parseable receipt, so that
fail-closed hardening does not change the results below. The final source-graph
hash is
`sha256:996fb0f50b7df91e3ef800af9756a1a4e39733cc615bfff37467143c22775586`.

| Provider | Model | Observed | Pass | Quality | p50 / p95 | Cost | Terminal state |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- |
| Anthropic | `claude-haiku-4-5-20251001` | 105 | 105 | 100 | 877 / 1,545 ms | $0.107265 | complete |
| Hugging Face | `Qwen/Qwen3-Coder-480B-A35B-Instruct` | 105 | 105 | 100 | 1,388 / 3,732 ms | $0.015454 | complete |
| OpenRouter | `google/gemma-4-26b-a4b-it` | 93 | 92 | 99 | 2,856 / 12,475 ms | $0.004933 | stopped unpriced |
| OpenAI | `gpt-4.1-nano` | 105 | 100 | 95 | 590 / 1,063 ms | $0.007500 | complete |
| xAI | `grok-4.3` | 105 | 93 | 89 | 2,809 / 10,366 ms | $0.111436 | complete |
| Together | `openai/gpt-oss-20b` | 105 | 68 | 65 | 1,163 / 2,581 ms | $0.006391 | complete |
| Fireworks | `accounts/fireworks/models/gpt-oss-120b` | 5 | 2 | 40 | 1,287 / 2,433 ms | $0.000879 | stopped unpriced |
| Cerebras | `gpt-oss-120b` | 105 | 41 | 39 | 345 / 670 ms | $0.042179 | complete |
| Groq | `llama-3.1-8b-instant` | 1 | 0 | 0 | 197 / 197 ms | N/A | stopped unpriced |

The combined scorecard contains 729 observations and cost $0.296036. Its JSON
hash is
`sha256:9653adc98dec1416ce4b218141389f29218e77e2d81029a9d55c02d1e05f205c`;
the Markdown projection hash is
`sha256:4b575555e2537ecb461ce7d112bbaacf0c689c9255ce0dc84b0cf4c005f09084`.
Every route remained review-gated because the scorecard intentionally keeps
streaming and offline request evidence as separate requirements.

## Review decisions

- Keep the current Anthropic, Hugging Face, OpenRouter, OpenAI, xAI, Together,
  Cerebras, and Groq catalog tool-format claims. The results expose route-level
  quality differences but do not contradict those claims with complete
  cross-mode evidence.
- Reject the generated Fireworks suggestions to flip the route from text-only
  to native. They were derived from only five observations before an overloaded
  response removed pricing evidence; one native and one text call passed, one
  call was malformed, and one was empty. That is not sufficient evidence for a
  catalog patch.
- Record OpenAI native parallel-tool calling as a stable 0/5 failure for this
  model. JSON and tagged-text alternatives passed, so this is not evidence that
  the route lacks tools altogether.
- Treat Gemini as N/A for this campaign: neither `GEMINI_API_KEY` nor
  `GOOGLE_API_KEY` was available. Unsupported and uncredentialed surfaces were
  not scored as failures.
- A streaming OpenAI smoke passed at the dispatch boundary, then stopped after
  the response omitted priced streaming usage. It is retained as a budget-path
  receipt but excluded from the priced aggregate.

The credential-free request audit rendered 7,136 catalog request shapes across
223 routes: 6,304 validation passes, zero validation failures, and 832
structural N/A cases. Its JSON hash is
`sha256:ca3902c6cb1d7437a08b21189db0ede2b3c57314e07f94b72ef735fe33c586ec`.

The model-adapter promotion contract already carries paired base and adapter
routes on current main. Its probe templates emit both `route_role = "base"` and
`route_role = "adapter"` rows for every required case, and `lora promote`
requires both `--base-probe-root` and `--probe-root`; no parallel receipt format
was added here.
