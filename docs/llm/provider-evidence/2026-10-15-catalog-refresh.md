# October 15, 2026 provider catalog evidence

The refresh adds 86 catalog routes, taking the packaged catalog from 313 to
399 rows. It covers current hosted chat families and native decision models.
It does not mirror every historical snapshot, batch-only route, image/video
generator, or account-specific deployment in each provider's index.

## Sources and scope

The audit fetched authenticated model indexes for Anthropic, OpenAI,
OpenRouter, Hugging Face, xAI, Together, Groq, Cerebras, Fireworks, Meta,
MiniMax, Z.ai, Moonshot, DeepInfra, SambaNova, NVIDIA, and Vercel. Google's
index uses the separate `v1beta/models` endpoint. Absence from an index was
not treated as proof of retirement.

The main hosted indexes returned 465 OpenRouter chat models, 25 OpenRouter
decision models, 268 Together models, 180 DeepInfra models, 134 Hugging Face
models, 81 NVIDIA models, and 470 Vercel models. Route IDs, context limits,
and prices were compared with the source fragments. Existing scheduled price
cards and empirically billed route prices were preserved instead of replacing
them indiscriminately with an aggregator's cheapest endpoint quote.

Primary references:

- [Meta's Spark Open announcement](https://research.meta.ai/blog/muse-spark-open)
  identifies the 816B-total, 42B-active model, its 1M context, and its hosted
  partners. [The model repository](https://huggingface.co/meta-models/Muse-Spark-Open-816B-A42B)
  supplies the released ID and license.
- [Meta's API rate card](https://dev.meta.ai/docs/pricing-rate-limits)
  distinguishes Wonder from Spark, standard from contributor traffic, and
  published account limits. Wonder Full has no public API route.
- [Google's Gemini 4 Argon reference](https://ai.google.dev/gemini-api/docs/latest-model)
  specifies the unified window, Interactions API, three thinking levels,
  implicit caching, and introductory rates through November 30. The direct
  catalog card restores regular rates on December 1; gateway cards are separate.
- [Google's deprecations](https://ai.google.dev/gemini-api/docs/deprecations),
  [OpenAI's deprecations](https://developers.openai.com/api/docs/deprecations),
  and [Anthropic's deprecations](https://platform.claude.com/docs/en/about-claude/model-deprecations)
  were checked. Gemini 2.5 Flash-Lite's earlier October 16 shutdown date was
  withdrawn. Existing-account restrictions remain; the row still recommends
  migration. Previously recorded provider-specific retirements remain intact.
- [TypeSafe's Jev 1.14 announcement](https://typesafe.ai/blog/jev-1-14) and
  [model reference](https://docs.typesafe.ai/models) distinguish preview from
  stable. Images, box answers, and extended context require experimental
  headers and are outside Harn's current text/JSON evaluator contract.
- [OpenRouter's chat index](https://openrouter.ai/api/v1/models) and
  [decision index](https://openrouter.ai/api/v1/models?output_modalities=decisions)
  are distinct catalogs. Jev's absence from the chat index is expected.
- [Vercel's model index](https://ai-gateway.vercel.sh/v1/models) and
  [evaluation reference](https://vercel.com/docs/ai-gateway/modalities/evaluation)
  distinguish model authors from serving providers.

The audit had no direct credentials for TypeSafe, Baseten, Nebius, Mistral,
Inception, or the account-specific cloud providers. These are verification
limits, not evidence that a provider is unavailable to other accounts.
Direct Mistral alias verification remains tracked by #9572.

Cohere's [current model index](https://docs.cohere.com/docs/models) also lists
North Mini Code and North Small Translate. Both evaluation routes are included
with published free-tier prices and unknown availability, without unverified
native-tool or reasoning claims. Their production deployments use separate
Model Vault or sales arrangements.

## Canonical product checks

`harn provider tool-probe` exercised native calls with streaming and
non-streaming responses, with text fallback disabled. The following 31 routes
returned a structured tool invocation in both modes:

| Family | Serving routes |
| --- | --- |
| Spark Open | OpenRouter, Fireworks, Together, Hugging Face, Vercel |
| Wonder | Meta, OpenRouter |
| Glimmer 30B | OpenRouter, Together, DeepInfra, NVIDIA |
| Gemini 4 Argon | Gemini Interactions, OpenRouter |
| Nemotron 3.5 Super VL | OpenRouter, Together, DeepInfra, NVIDIA, Vercel |
| Step 5 Preview | OpenRouter, DeepInfra |
| Ember 1 | OpenRouter, Fireworks |
| Ling 3.1 Flash | OpenRouter, DeepInfra |
| Inkling | OpenRouter, Together, Fireworks |
| Inkling Small | OpenRouter, DeepInfra |
| Nemotron 3.5 Lightning | OpenRouter, DeepInfra |

Real `harn run` agent loops defined a read-only tool that returned a fresh
marker unknown to the model. Success required exactly one handler execution,
the actual marker in the final response, and a completed terminal state.
This passed on the first twelve Spark Open/Wonder/Glimmer/Argon/Super VL
routes selected for agent checks, plus OpenRouter LFM 2.5 and Qwen Max Prime.
It also passed on OpenRouter Pareto 26.9, Perceptron Mk1.5, Aion 3.5,
Solar Mini 4 and Pro 4, MiMo V2.6 Pro UltraSpeed, Qwen 3.8 Flash and Omni
Flash, Qwen 3.8 2.4T, Hy4 Preview, Nex N2.5 Pro, Granite 4.2, Spark 1.3,
and Seed 2.1 Turbo.

Vision checks supplied solid red and blue image bytes with identical prompts
that did not disclose the expected color. Both controls passed on OpenRouter
Spark Open, Wonder, Glimmer, Super VL, direct Meta Wonder, Fireworks Spark
Open, and direct Gemini Argon. Additional hosted-family vision checks use the
same two-image control and passed on all eight additional routes after the
Spark 1.3 mandatory-reasoning correction. These checks establish image ingestion and basic
recognition, not document, audio, video, or general reasoning quality.

The native evaluator answered four questions in a single physical attempt
on all 21 cataloged OpenRouter decision routes. Every response classified the
positive boolean true, its negative control false, the parcel color blue,
and satisfaction happy. Receipts recorded `physical_attempts = 1` and actual
served model IDs. `~typesafe/jev-latest` returned
`typesafe/jev-1.14-20261014`, while direct TypeSafe's documented stable alias
still targets 1.13. A neighboring nonexistent Jev ID returned HTTP 400.

## Failures and resulting contracts

- Groq Spark Open returned `model_terms_required`. Its 128K preview route
  retains unknown availability and does not claim verified native tools.
- Baseten Spark Open has no live credential proof. Modal's endpoint model ID
  is workspace-specific, so the provider guide describes an overlay rather
  than inventing a global ID. Nebius is reachable through OpenRouter; no
  unverified direct ID was added.
- Vercel Wonder, Glimmer, Argon, and the new pinned Jev versions returned
  paid-credit access errors on this account. Their availability is unknown.
- Vercel Laya, d1 Ultra, and Decider answered raw unfiltered requests, but
  rejected the evaluator's required zero-retention routing filter. They retain
  unknown availability. The refresh does not remove privacy controls.
- OpenRouter Spark 1.3 Contributor was excluded by the account's training
  restriction. Its catalog row explicitly records that upstream training
  policy and unknown availability.
- The free Super VL route was also excluded by the account's privacy guard;
  the paid route passed. The free row retains unknown availability.
- Mercury 2.5 returned upstream 502 errors for tools and effort probes. Its
  route is cataloged with unknown availability and no verified native tools.
- Qwen Max Prime rejected forced tool choice with an explicit thinking-mode
  error. Automatic tool choice returned the requested native call and a real
  agent loop completed. Its capability rule permits `auto` only.
- LFM's non-streaming 256-token tool probe exhausted its budget. Streaming
  succeeded, and a 4096-token real agent loop executed the handler and finished.
- Qwen 3.8 Flash requires the total completion budget to exceed its enabled
  thinking budget. A 4096-token agent probe hit that boundary; 8192 completed.
  MiMo UltraSpeed's first second-turn request returned 520; two later complete
  agent runs passed. These transient failures remain part of the evidence.
- Aion 3.5 Mini returned an invented argument key and failed the marker task.
  Its row does not advertise native tools. A successful HTTP response is not
  proof of a usable tool contract.
- Choice-bound probes found Tev's actual ceiling is 20, and Solar's is 26.
  Their capability limits use those observed ceilings. Most other decision
  routes returned full distributions at 255 choices and ten score levels.
  Some distributions produced low-confidence outcomes, which is not a schema
  failure. ScaleDown's long-candidate response exceeded the evaluator's
  conservative token projection and was settled as a provider-contract budget
  cut; its small four-question check passed.
  Tev returned distributions at 20 choices and rejected 21; both Solar routes
  returned distributions at 26 and rejected 27. Ten score levels succeeded in
  those boundary requests.
  Vercel's unfiltered Liquid and Decider routes accepted 255 choices and ten
  score levels. Laya accepted 64 short choices, but 128 and 255 returned 413;
  its adapter uses a conservative tested bound of 64. Laya separately accepted
  ten score levels. These raw successes do not override the evaluator's ZDR
  refusal.
- New reasoning routes were probed with an ungated seven-rung effort sweep.
  Acceptance alone does not establish that every accepted synonym is honored;
  curated ladders stay within documented semantics. Mandatory reasoning
  refusals are preserved rather than silently disabling it.

The routed Spark Open probes also exposed a runtime ownership bug: a provider
wire ID such as `meta-models/Muse-Spark-Open-816B-A42B` was inferred as
OpenRouter after normalization. Resolution now reuses the catalog's reverse
wire lookup under the explicit provider. The successful Together and Hugging
Face product runs exercise that correction; an unregistered neighboring wire
ID remains rejected.

OpenRouter's alpha decisions transport does not apply chat-only data controls
that have not been verified on that endpoint. Its evaluation receipts record
`no_control_available`. Vercel's native evaluator continues to require its
existing zero-retention and no-training provider filters.

## Artifacts and limits

Raw audit responses and live receipts are in the task's temporary evidence
directory, `/tmp/harn-provider-refresh/`. They contain synthetic prompts and
no credentials. The immutable CLI snapshot used for the live checks has
SHA-256 `eed41700a168d4898ea96301bb3d99a033c931cf1052ebc00991b2e022b6ce4d`.
Requests used the current generated catalog; the later family checks supplied
the same capability fragments as a project overlay while the final packaged
binary was building. Packaged capability regressions verify those projections.

The completed image census covers 37 working routes, with both colors checked
on each. The free Super VL endpoint was attempted and blocked by the account's
privacy restriction rather than counted as an image success.

Live checks are compatibility smoke tests, not a model-quality ranking.
NVIDIA and Hugging Face calls without published catalog prices are unpriced,
not free. Explicit tool/evaluator receipts recorded about $0.17 in charges.
Total live-probe spend is estimated below $1; image checks and effort sweeps
lack complete billing receipts, and unpriced routes are excluded from that
recorded subtotal.
