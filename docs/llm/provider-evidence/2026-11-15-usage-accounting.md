# November 15, 2026 usage-accounting audit

The [remaining accounting audit](https://github.com/burin-labs/harn/issues/7320)
still has 14 unverified fields across nine providers. The complete source
census contains 39 OpenAI-SSE providers and 39 distinct audit entries: 30
fully verified, four with streaming evidence only, and five unverified.
No declaration or verified evidence entry changes in this review.

The issue's fourth acceptance criterion permits a bounded expiry renewal with
a dated explanation of the blockers. This review renews the research queue
from November 14 through November 22. It does not complete the provider audit.
The same catalog gate still rejects missing coverage, unsupported declarations,
duplicate entries, and an expired queue.

## Remaining contracts

The official endpoint references below were reviewed on November 15. An
omitted field in documentation is not evidence that accounting is unsupported.
Streaming support and cache support are separate claims.

| Provider | Unverified fields | Exact remaining boundary |
| --- | --- | --- |
| Atlas | Cache | The [protocol reference](https://www.atlascloud.ai/docs/en/llm-protocols) promises final streaming usage, already verified. It does not establish cache-counter emission across the catalog's model routes. |
| Cloudflare AI Gateway | Cache, stream | The configured default is the account-scoped [Workers AI compatibility endpoint](https://developers.cloudflare.com/workers-ai/configuration/open-ai-compatibility/). Its [February 17 changelog](https://developers.cloudflare.com/workers-ai/changelog/#2026-02-17) documents final streaming usage, and [cache counters](https://developers.cloudflare.com/workers-ai/features/prompt-caching/) apply to selected models. The issue's documented next proof is an exact configured endpoint/model live observation, including absent-versus-zero counters; it has not run. |
| Cohere | Cache, stream | The [compatibility API](https://docs.cohere.com/docs/compatibility-api) documents the catalog's compatibility endpoint and streamed text. Its examples do not establish final usage or cache fields for that route; the native API is a different contract. |
| Hugging Face | Cache | The [chat-completion reference](https://huggingface.co/docs/inference-providers/tasks/chat-completion) spans multiple inference providers. The audit's earlier observed DeepInfra cache hit does not qualify every backend. Streaming remains verified. |
| Hunyuan | Cache | The [OpenAI-compatible reference](https://cloud.tencent.com/document/product/1729/111007) qualifies the existing streaming declaration. It does not settle cache-counter emission on that endpoint; TokenHub is a separate platform. |
| Hyperbolic | Cache | The [current endpoint reference](https://www.hyperbolic.ai/docs/inference/text-apis) shows optional cached-token details and explicitly says usage fields vary by model. An exact-route observation must preserve that conditionality. Streaming remains verified. |
| Local | Cache, stream | The configured endpoint is supplied by the operator. One local server's behavior cannot establish a universal declaration for arbitrary upstreams. |
| Parasail | Cache, stream | The [chat-completion reference](https://docs.parasail.io/parasail-docs/api-reference/chat-completions) documents the catalog endpoint, ordinary response counters, and SSE termination. It does not establish final streamed usage or cache emission. |
| SiliconFlow | Cache, stream | The [global chat-completion reference](https://docs.siliconflow.com/en/api-reference/chat-completions/chat-completions) documents ordinary usage counters. A selected CN model example cannot establish terminal usage and cache behavior across the global route. |

## Measurement and limits

The standard credential file's configured authentication-name census found a
Hugging Face credential name among these nine providers. No credential values
were printed. The vault authorization attempt timed out, so vault coverage is
unmeasured, not empty. No live provider request or paid inference ran during
this review. Previously retained provider observations remain evidence only
for their recorded routes.

A native CLI loopback probe separately found an accounting bug: an unknown
streaming declaration stopped at the content finish frame and discarded a
later receipt. The identical receipt reported 37 input, 11 output, seven
cached input, and 48 total tokens when support was explicitly declared; with
no declaration it was incorrectly lost. The separate runtime correction
retains the observed receipt while keeping missing or empty receipts unknown,
an explicit zero reported, and a total-only receipt distinct from invented
input or output counts. Six actual loopback requests reached their controlled
endpoint, with zero authorization headers and zero external provider calls.
That correction supplies no new external provider declaration.

Completing the queue requires exact-route documentation or recorded wire
evidence for the missing fields, including absent-versus-zero controls, and
an explicit contract for configurable or model-dependent accounting. Unknown
support must remain unknown until that evidence exists.
