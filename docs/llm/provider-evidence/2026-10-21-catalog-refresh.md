# October 21, 2026 read-only provider catalog evidence

This incremental refresh adds 20 routes to the October 15 catalog. All new
routes have `availability = "unknown"` and empty verified capabilities.
Exact capability guards prevent broader, previously probed family rules from
advertising native tools, vision, video, or effort control on these new IDs.
Their JSON/text-tool fallback is also unverified and disabled. The providers
advertise more features than Harn has established here; this is an evidence
limit, not a statement that the upstream features are absent.

No inference requests were made. The user's no-paid-probes instruction
prevents the live capability checks required by AGENTS.md. New aliases
`minimax31` and `mercury3` are explicit opt-ins; operational defaults, pinned
older aliases, historical rows, and previously measured prices remain intact.

## Official read-only indexes

All reads below were GETs performed on 2026-10-21, using existing
provider credentials from the host's documented dotenv sources in memory.
Nineteen catalogs were read with authentication, Inception was public, and five
other endpoints required credentials that were not available. No new
credential was created and no access configuration was changed.

| Provider | Exact endpoint | Result |
| --- | --- | --- |
| anthropic | https://api.anthropic.com/v1/models?limit=1000 | 14 model records (authenticated) |
| openai | https://api.openai.com/v1/models | 133 model records (authenticated) |
| openrouter | https://openrouter.ai/api/v1/models | 468 model records (authenticated) |
| together | https://api.together.xyz/v1/models | 269 model records (authenticated) |
| deepinfra | https://api.deepinfra.com/v1/openai/models | 177 model records (authenticated) |
| fireworks | https://api.fireworks.ai/inference/v1/models | 22 model records (authenticated) |
| vercel_ai_gateway | https://ai-gateway.vercel.sh/v1/models | 482 model records (authenticated) |
| nvidia | https://integrate.api.nvidia.com/v1/models | 83 model records (authenticated) |
| minimax | https://api.minimax.io/v1/models | 9 model records (authenticated) |
| zai | https://api.z.ai/api/paas/v4/models | 11 model records (authenticated) |
| moonshot | https://api.moonshot.ai/v1/models | 4 model records (authenticated) |
| xai | https://api.x.ai/v1/models | 14 model records (authenticated) |
| groq | https://api.groq.com/openai/v1/models | 12 model records (authenticated) |
| cerebras | https://api.cerebras.ai/v1/models | 2 model records (authenticated) |
| mistral | https://api.mistral.ai/v1/models | 62 model records (authenticated) |
| sambanova | https://api.sambanova.ai/v1/models | 6 model records (authenticated) |
| huggingface | https://router.huggingface.co/v1/models | 135 model records (authenticated) |
| gemini | https://generativelanguage.googleapis.com/v1beta/models?pageSize=1000 | 63 model records (authenticated) |

| meta | https://api.meta.ai/v1/models | 9 model records (authenticated) |
| deepseek | https://api.deepseek.com/v1/models | HTTP 401; no credential available |
| cohere | https://api.cohere.ai/v1/models | HTTP 401; no credential available |
| dashscope | https://dashscope-intl.aliyuncs.com/compatible-mode/v1/models | HTTP 401; no credential available |
| baseten | https://inference.baseten.co/v1/models | HTTP 401; no credential available |
| inception | https://api.inceptionlabs.ai/v1/models | 4 model records (public) |
| nebius | https://api.tokenfactory.nebius.com/v1/models | HTTP 401; no credential available |

A missing model in one index does not establish retirement. No new retirement
is inferred from those absences. Anthropic's [model overview](https://platform.claude.com/docs/en/models/overview)
and [deprecations](https://platform.claude.com/docs/en/about-claude/model-deprecations),
Google's [models](https://ai.google.dev/gemini-api/docs/models), and Groq's
[deprecations](https://console.groq.com/docs/deprecations) were re-read. Their
current direct model generations and dated retirements are already represented.
Groq now publicly documents Compound's September 21 retirement; its older
email-only source note is updated without changing its historical row or
Harn's existing suggested replacement.
Together's [deprecation schedule](https://docs.together.ai/docs/deprecations)
was re-read; the October 22 retirements and replacements fixed in #10261 are
preserved. This work does not change the .172 release or its publication lane.

## New routes and route-specific price provenance

Prices are USD per million tokens, input/output/cache-read. An em dash means
no cache quote was supplied, not free cache service. Public catalog prices
are quotes, not evidence of billed spend or account entitlement.

| Catalog ID | Provider wire ID | Context | Input / output / cache | Source |
| --- | --- | ---: | --- | --- |
| `minimax/minimax-m3.1-flash:free` | `minimax/minimax-m3.1-flash:free` | 1000000 | 0.0 / 0.0 / — | [official source](https://openrouter.ai/api/v1/models) |
| `deepseek/deepseek-v4.1-flash-1017` | `deepseek/deepseek-v4.1-flash-1017` | 1048576 | 0.3 / 1.2 / 0.006 | [official source](https://openrouter.ai/api/v1/models) |
| `elorian/glimpse` | `elorian/glimpse` | 262144 | 0.6 / 3.0 / 0.06 | [official source](https://openrouter.ai/api/v1/models) |
| `xingchen-agi/xing4.0-29b-a4b:free` | `xingchen-agi/xing4.0-29b-a4b:free` | 262144 | 0.0 / 0.0 / — | [official source](https://openrouter.ai/api/v1/models) |
| `reflection/beam-1.0-501b-a23b` | `reflection/beam-1.0-501b-a23b` | 1048576 | 0.3 / 1.0 / 0.04 | [official source](https://openrouter.ai/api/v1/models) |
| `inception/mercury-3` | `inception/mercury-3` | 1000000 | 0.1 / 0.35 / 0.01 | [official source](https://openrouter.ai/api/v1/models) |
| `unbiased/pareto-26.10` | `unbiased/pareto-26.10` | 1048576 | 0.8 / 3.2 / 0.03 | [official source](https://openrouter.ai/api/v1/models) |
| `vercel/minimax/minimax-m3.1-flash` | `minimax/minimax-m3.1-flash` | 1000000 | 0.0 / 0.0 / — | [official source](https://ai-gateway.vercel.sh/v1/models) |
| `vercel/deepseek/deepseek-v4.1-flash-1017` | `deepseek/deepseek-v4.1-flash-1017` | 1048576 | 0.15 / 0.6 / 0.003 | [official source](https://ai-gateway.vercel.sh/v1/models) |
| `vercel/deepseek/deepseek-v4.1-flash-1017-fast` | `deepseek/deepseek-v4.1-flash-1017-fast` | 1048576 | 0.6 / 2.4 / 0.014 | [official source](https://ai-gateway.vercel.sh/v1/models) |
| `vercel/inception/mercury-3` | `inception/mercury-3` | 1000000 | 0.1 / 0.35 / 0.01 | [official source](https://ai-gateway.vercel.sh/v1/models) |
| `vercel/elorian/glimpse` | `elorian/glimpse` | 220000 | 0.6 / 3.0 / 0.06 | [official source](https://ai-gateway.vercel.sh/v1/models) |
| `MiniMax-M3.1-Flash` | `MiniMax-M3.1-Flash` | 1000000 | 0.15 / 0.6 / 0.015 | [official source](https://www.minimax.io/blog/minimax-m31f) |
| `mercury-3` | `mercury-3` | 1000000 | 0.2 / 0.7 / 0.02 | [official source](https://docs.inceptionlabs.ai/get-started/models) |
| `together/deepseek-ai/DeepSeek-V4.1-Flash-1017` | `deepseek-ai/DeepSeek-V4.1-Flash-1017` | 1048576 | 0.3 / 1.2 / 0.006 | [official source](https://docs.together.ai/docs/serverless/models) |
| `deepinfra/deepseek-ai/DeepSeek-V4.1-Flash-1017` | `deepseek-ai/DeepSeek-V4.1-Flash-1017` | 1048576 | 0.2 / 0.6 / 0.006 | [official source](https://api.deepinfra.com/v1/openai/models) |
| `huggingface/deepseek-ai/DeepSeek-V4.1-Flash-1017` | `deepseek-ai/DeepSeek-V4.1-Flash-1017` | 1048576 | 0.3 / 1.2 / — | [official source](https://router.huggingface.co/v1/models) |
| `huggingface/reflection/Beam-1.0-501B-A23B` | `reflection/Beam-1.0-501B-A23B` | 1048576 | 0.4 / 1.2 / — | [official source](https://router.huggingface.co/v1/models) |
| `accounts/fireworks/models/deepseek-v4p1-flash-1017` | `accounts/fireworks/models/deepseek-v4p1-flash-1017` | 1048576 | unpriced | [authenticated index](https://api.fireworks.ai/inference/v1/models) |
| `accounts/fireworks/models/minimax-m3p1-flash` | `accounts/fireworks/models/minimax-m3p1-flash` | 1048576 | unpriced | [authenticated index](https://api.fireworks.ai/inference/v1/models) |

### DeepSeek snapshot and alias semantics

The [direct pricing page](https://api-docs.deepseek.com/quick_start/pricing/)
and [first-call guide](https://api-docs.deepseek.com/) identify `deepseek-flash`
as DeepSeek-V4.1-Flash-1017, and the old `deepseek-v4-flash` and
`deepseek-v4-flash-vision-exp` wire names as compatibility redirects. Harn
updates the existing Flash display name and legacy note; the existing wire
IDs and historic prices stay addressable. It does not invent a direct dated
`deepseek-v4.1-flash-1017` API ID. Direct `deepseek-v4-pro` still names Pro 0813.
The old hosted V4.1 Flash rows remain distinct: no evidence here establishes
that their undated IDs redirect to 1017 on every provider.

The new OpenRouter 1017 card uses peak $0.30/$1.20/$0.006 and the index's
weekday UTC windows (01:00–04:00 and 06:00–10:00). Its observed off-peak quote
$0.15/$0.60/$0.003 is not baked in as an all-day rate. The Vercel standard
route has its own $0.15/$0.60/$0.003 peak card and half-price off-peak windows;
its fast route is a separate $0.60/$2.40/$0.014 card. Vercel's public standard
quote varies by serving provider/region and is not a guaranteed regional rate.
DeepInfra's base quote is $0.20/$0.60/$0.006; its index also advertises a 51%
discount without an end date, which is not turned into a permanent rate here.
Hugging Face lists Novita for 1017 and Baseten for Beam with the quoted prices
in the table; those are serving-route quotes, not direct author prices.

The Together [serverless table](https://docs.together.ai/docs/serverless/models)
independently lists the exact dated 1017 ID and $0.30/$1.20/$0.006 prices. Its
documentation says 1,000,000 context while its authenticated index says
1,048,576; the row follows the machine-readable index and retains unknown
availability (the index also reports `running: false`). Fireworks' authenticated
index confirms `accounts/fireworks/models/deepseek-v4p1-flash-1017` and
`accounts/fireworks/models/minimax-m3p1-flash`, each with 1,048,576 context,
but no price. Both are included as unpriced routes, rather than borrowing a
gateway price. Fireworks' [batch compatibility guide](https://docs.fireworks.ai/guides/batch-inference),
read 2026-10-21, warns that new models may not yet support batch submission.
The index does not establish batch compatibility for either new route, so their
exact capability rule explicitly opts out of the legacy provider-wide batch
default; this is withheld support, not evidence that the upstream rejects batch.
NVIDIA's index
still lists only the undated Flash route. Those limitations are explicit
rather than silently claiming route completeness.

### MiniMax M3.1 Flash

The [October 19 release](https://www.minimax.io/blog/minimax-m31f) supplies the
$0.15/$0.60/$0.015 direct flat card and states that weights will be released
later. `open_weight = false` therefore takes precedence over Vercel's
`open-weights` tag. The [OpenAI SDK reference](https://platform.minimax.io/docs/api-reference/text-openai-api)
supplies the exact `MiniMax-M3.1-Flash` ID, 1,000,000 context, text/image/video
input, mandatory thinking, and low/medium/high/xhigh/max effort (default max).
It says temperature and top-p are ignored; the exact direct rule strips those
controls, requires completion-token budgeting, and does not allow disabling
reasoning. These are request restrictions, not verified positive capabilities.

OpenRouter currently indexes only `minimax/minimax-m3.1-flash:free`; no paid
slug is invented. Vercel independently quotes zero input/output prices for its
unsuffixed route. Both remain unknown availability, without assuming that the
account permits free routing, training, or provider retention policies.

### Mercury 3 and other hosted additions

Inception's [model reference](https://docs.inceptionlabs.ai/get-started/models)
identifies direct `mercury-3`, 1M context, 131,072 output, text input, tools,
structured outputs, and instant/low/medium/high effort. Its regular card is
$0.20/$0.70/$0.02; the 50% launch promotion is recorded separately as
$0.10/$0.35/$0.01 with an October 28 review date and no invented end date.
OpenRouter and Vercel quote their own current launch rates. No default or
older Mercury row is replaced; no native tool or effort success is claimed.
Enterprise-only Mercury Voice and the distinct Mercury Decide endpoint are
outside this chat-route increment and require access/transport verification.

Official weight repositories confirm released [Beam weights](https://huggingface.co/reflection/Beam-1.0-501B-A23B),
[Xing4 weights](https://huggingface.co/XingChen-AGI/Xing4.0-29B-A4B), and
[DeepSeek 1017 weights](https://huggingface.co/deepseek-ai/DeepSeek-V4.1-Flash-1017).
The OpenRouter index advertises tools and reasoning on Xing4 and Beam,
image/video plus tools on Glimpse, and image plus tools on Pareto 26.10.
The already cataloged Moondream 3.1 advertises image reasoning without tools. HF's index additionally
lists Beam. Glimpse's Vercel route is 220,000 context and has no tools in its
parameter list; OpenRouter quotes 262,144 and lists tools. The different
windows are kept rather than normalized across routes. Pareto 26.9 and
Moondream Preview are retained; neither absence nor a successor name is a
retirement notice. Direct Elorian, Reflection, XingChen, and Moondream globally
callable IDs were not established, so no provider or credential is invented.

## Verification limit

The packaged-catalog regression tests check real membership (including
nonexistent neighboring negative controls), unchanged older aliases and
contrasting previously verified family capabilities, exact wire IDs, unknown
availability, withheld capability promotion, and peak/off-peak/promotion
boundaries. Generators and their check targets verify all public projections.
No live generation, tool, image, effort, or billing check is represented by
these deterministic tests. Follow-up capability promotion needs authorized
live validation on each serving route.

## Existing route and alias reconciliation

OpenRouter's authenticated index still lists all three Gemini 2.5 chat routes
with `expiration_date: null`, contradicting the October 1 index's October 20
sunset announcement. Harn removes the stale hard cutoff from its existing
Flash row, keeps the prior announcement in its deprecation note, retains the
migration recommendation and price history, and marks availability unknown.
A successful list operation does not establish that a post-sunset call works.
A separate downstream host correction moves its generic `or-gemini-flash`
alias to Harn's already recommended 3.5 Flash-Lite route; the historical 2.5
row is retained.

Meta's authenticated index confirms its existing Spark/Wonder direct IDs;
Inception's public index independently confirms Mercury 3, Voice, and Decide.
The direct Mistral index confirms `codestral-latest`, `mistral-code-latest`, and
`mistral-code-fim-latest` as `codestral-2508` aliases; `mistral-large-4` and
`mistral-large-4-0` are reciprocal IDs with billing model `mistral-large-4-0`.
The existing Harn wire ID is valid. `mistral-medium-3-5` is also indexed with
its latest/Vibe/Magistral aliases and 262,144 context. This supplies read-only
alias evidence previously missing from the October 15 audit, not a new live
capability or billing probe. No direct alias is retargeted based on name shape.
Protected indexes also confirm the current direct Anthropic, Z.ai, Moonshot,
Cerebras and OpenAI families already present in the catalog. Image, audio,
embedding, moderation, realtime, dedicated deployments and older unbundled
snapshots remain outside this incremental chat-route refresh.
