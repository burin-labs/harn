# Provider catalog refresh workflow

`scripts/update_provider_catalog.harn` is the Harn-native workflow that
periodically collects model availability, pricing, and capability
signals from provider sources, normalizes them, and emits:

- a markdown **drift report** under
  `.harn-runs/provider_catalog/drift-report.md`;
- a TOML **candidate patch** under
  `.harn-runs/provider_catalog/candidate.toml`.

The workflow never mutates the shipped catalog. The patch is a review
aid: diff it against the TOML fragments under
`crates/harn-vm/src/llm/catalog_sources/`, the capability fragments under
`crates/harn-vm/src/llm/capability_sources/`, or your project's
`harn.toml` overlay before landing changes.

Keep the provider's durable rate card in the main pricing fields. Put a dated
discount in `pricing.promotions`. Harn uses an active promotion for cost
estimates and returns to the durable rate when it ends. Use `review_after` when
a provider gives a minimum duration but no firm end date.

The markdown report includes aggregator discoveries for awareness, but the
candidate TOML contains only provider-owned, high-confidence changes and
additions with actionable pricing or capability metadata. This
keeps broad discovery APIs such as OpenRouter's useful without turning bare
model IDs or uncurated mirrors into apparent review-ready catalog rows.
Catalog identity is always the `(provider, model id)` pair; equal model ids on
different providers are compared independently.

## Running the workflow

```bash
# Default: replay against bundled fixtures, write report + candidate
# under .harn-runs/provider_catalog/.
harn run scripts/update_provider_catalog.harn

# Live mode: hit real provider sources. Key-required adapters attach
# the configured auth header from env vars; missing keys produce a
# "skipped" diagnostic in the report instead of a failure.
harn run scripts/update_provider_catalog.harn -- --live

# CI gate: same fixture replay, but compare against the committed
# goldens at scripts/provider_catalog_fixtures/expected_*.
harn run scripts/update_provider_catalog.harn -- --check

# Refresh the committed goldens after intentional adapter changes.
harn run scripts/update_provider_catalog.harn -- --check --update
```

The CI gate is wired into `make check-provider-catalog-drift` and runs
from `make all`.

## Trusted change notices

`scripts/provider_catalog_notice.harn` handles narrow provider change notices
between broad refresh runs. It consumes a transport-neutral JSON record:

```json
{
  "source_id": "provider-pricing-2026-08",
  "retrieved_at": "2026-07-28T12:00:00Z",
  "provider": "anthropic",
  "text": "The original notice text, if available.",
  "source_url": "https://provider.example/immutable-notice",
  "provenance": "verified_link"
}
```

An email label, webhook, or document-store adapter only needs to write this
shape. `provenance` is `authenticated`, `verified_link`, `unverified`, or
`failed`; the latter two fail closed. At least one of `text` or an immutable
`source_url` is required.

The model is limited to a schema-constrained extraction:

- `price`, `promotion`, `retirement`, `endpoint`, and `capability` are distinct typed
  variants;
- provider, model id, effective date, old/new values, and supporting evidence
  are retained;
- the model never receives a file mutation tool and never emits TOML.

Deterministic code then resolves exactly one provider and model row in the
loaded catalog, verifies the old value, finds exactly one owning source table,
and applies the constrained edit. Missing or duplicate identities are
rejected. A model absent from an unsupported provider records a no-op; a new
model on a supported provider produces an incomplete proposal listing context,
pricing, capability, and routing verification still required.
Capability edits target the generated capability matrix's owning fragments,
not legacy model tags. They require one exact `model_match` rule; a
wildcard-derived family rule is deliberately rejected as ambiguous rather than
silently changing sibling models.

Use a checked extraction fixture to inspect the plan without an LLM call:

```bash
harn run scripts/provider_catalog_notice.harn -- \
  --notice scripts/provider_catalog_notice_fixtures/carried-update.json \
  --extraction scripts/provider_catalog_notice_fixtures/carried-update.json
```

When either input is outside the worktree, grant its containing directory
before the script path. Keep the run sandboxed and repeat `--read-only-root`
when the notice and checked extraction come from different directories:

```bash
notice_path=/path/to/notice.json
extraction_path=/path/to/extraction.json
harn run \
  --read-only-root "$(dirname "$notice_path")" \
  --read-only-root "$(dirname "$extraction_path")" \
  scripts/provider_catalog_notice.harn -- \
  --notice "$notice_path" \
  --extraction "$extraction_path"
```

For a live extraction, omit `--extraction` and optionally choose `--provider`
and `--model`. The extraction has a $0.10 per-notice cap. By default the script
only writes an idempotent local receipt below
`.harn/provider-catalog-notices/`. `--apply` requires a clean, dedicated
worktree and runs catalog generation plus drift, artifact, documentation,
support, and capability checks. `--open-pr` uses a stable branch derived from
the notice digest, pushes it, and opens a **draft** PR. It never enables merge
or auto-merge. A repeated notice reuses the same digest and returns an existing
PR instead of creating another.

```bash
git worktree add ../harn-provider-notice origin/main
(
  cd ../harn-provider-notice
  make setup
  notice_path=/path/to/notice.json
  harn run \
    --read-only-root "$(dirname "$notice_path")" \
    scripts/provider_catalog_notice.harn -- \
    --notice "$notice_path" \
    --repo-root "$PWD" \
    --apply \
    --open-pr
)
```

Receipts stay below the dedicated worktree by default. If an adapter instead
passes an absolute external `--output-dir`, create that directory first and
grant that exact directory with `--write-root` before the script path. A
read-only root does not authorize receipt writes.

The optional cron package in
`examples/triggers/provider-catalog-notice/` shows how a public adapter can
place a neutral notice file and invoke this workflow. The scheduler needs only
the worktree and draft-PR authority granted to that process; private mailbox or
product state is not part of the workflow contract.

## Measuring tool-call routes

Use the built-in probe for one route. It sends the request through the same
provider adapter, request builder, stream parser, and response normalizer as
`harness.llm.call`. No probe script or temporary Harn file is needed.

```bash
# Inspect the request without calling the provider.
harn provider tool-probe openai --model gpt-5.6-sol --dry-run-request

# Check non-streaming and streaming native tool calls.
harn provider tool-probe openai --model gpt-5.6-sol --mode both
```

The probe reads the provider's normal API-key environment variable. Pass
`--base-url` only to test a raw compatible endpoint; that override bypasses the
provider adapter. Each JSON result records the observed tool-call shape,
latency, token use, and catalog-priced cost.

Use the spend-capped campaign to measure several routes or repeat a stochastic
claim. Dry-run mode performs request and credential readiness checks without
provider calls:

```bash
harn run --no-sandbox scripts/provider_tool_probe_campaign.harn -- \
  --route openai:gpt-4.1-nano \
  --behavior provider_tool_probe \
  --repeat 3 \
  --max-cost-usd 5 \
  --output-dir .harn-runs/provider-tool-probe-campaign/openai-nano
```

Add `--live` only after reviewing that plan. Live repeats are executed as
separate probe processes so the campaign accounts for every physical request
before starting the next one. The campaign stops when the observed
runtime-priced cost reaches `--max-cost-usd`; it also fails closed after an
unpriced case instead of continuing with an unknown ledger. `--max-probes`
limits selected route/behavior/mode units independently of the dollar cap.

Each live output directory contains:

- `report.json` and `report.md`, including parseability, empty completions,
  latency, request/retry incidence, rate limits, token usage, and spend;
- the individual `tool-probe-*.json` provider receipts;
- `scorecard-inputs.json`, a manifest binding every input hash to the Harn
  source revision, exact campaign-source hash, and provider-catalog hash;
- deterministic `scorecard.json` and `scorecard.md` artifacts generated by
  `harn provider tool-scorecard`;
- `scorecard-receipt.json`, which hashes the inputs and both projections.

The tool probe deliberately performs no automatic retries, so `retry_count` is
zero and `request_attempt_count` is the physical request count. A 429 is
recorded separately as a rate-limited case. Suggested catalog changes in the
scorecard remain review-only data patches; the campaign never edits catalog
TOML.

## Measuring portable option claims

Use `harn provider option-probe` when the catalog says an endpoint accepts or
rejects one of Harn's portable generation options. The command makes one small
request through the normal provider adapter and compares the provider's answer
with the exact catalog field:

```bash
# No provider call. Shows the endpoint, claim, field, and request count.
harn provider option-probe anthropic \
  --model claude-opus-4-7 \
  --option temperature \
  --plan --json

# One live request. Exit 1 means drift; exit 2 means no provider measurement.
harn provider option-probe anthropic \
  --model claude-opus-4-7 \
  --option temperature \
  --fail-on-drift --json
```

The typed option vocabulary is `temperature`, `top_p`, `top_k`, `seed`,
`frequency_penalty`, `presence_penalty`, and `stop`. Each report names the
provider and model endpoint, catalog field, declared value, request count,
measured count, provider verdict, and one of `match`, `drift`, or `unmeasured`.
Authentication, throttling, missing models, and local admission failures are
`unmeasured`; they cannot turn an absent provider observation into a match.
So are unrelated terminal outcomes such as content-policy blocks. Only the
provider error taxonomy's structured `invalid_request` reason proves that the
wire shape was rejected.

Normal calls use the catalog to reject or remove unsupported options before
egress. A truthful negative probe must let its selected option reach the
provider, or it can only confirm the catalog against itself. The CLI selects
probe authority in a typed async-task scope, then option extraction captures it
in the resolved call contract and carries it through spawned transport work.
It suspends shaping for the selected option only; sibling calls and unrelated
options keep normal catalog policy. Pass `--gated` for a confirm-only run that
leaves every guard enabled.

An `accepted` verdict proves that the endpoint accepted a meaningful,
non-default value on the wire. It does not prove that a provider honored the
value semantically; providers that silently ignore unknown fields require a
separate output-sensitive conformance test.

This distinction matters because provider APIs change at model boundaries.
Anthropic's current [Messages API
reference](https://platform.claude.com/docs/en/api/messages/create) says models
after Claude Opus 4.6 reject non-default `temperature`, `top_p`, and `top_k`.
Google's [model resource](https://ai.google.dev/api/models) exposes whether a
Gemini model uses top-k sampling; an empty `topK` means requests may not set it.
The catalog records those endpoint-specific facts instead of treating a field
present in an SDK type as universally supported.

The existing spend-capped campaign executes every portable option without a
second orchestration path:

```bash
harn run --no-sandbox scripts/provider_tool_probe_campaign.harn -- \
  --catalog-routes \
  --exclude-local \
  --behavior provider_option_probe \
  --max-probes 20 \
  --max-cost-usd 2 \
  --output-dir .harn-runs/provider-option-probes
```

Review the dry run, then add `--live`. A live option campaign is incomplete
unless it contains at least one measured-supported and one measured-unsupported
control. Its `claim_controls` receipt prints both counts and the unmeasured
count, so zero observations cannot read as healthy. Individual
`option-probe-*.json` receipts join the same catalog hash, runtime fingerprint,
sharding, credential-name readiness, and budget ledger as tool-call receipts.
Credential-missing and otherwise skipped option cells count as unmeasured in a
live campaign, so partial endpoint coverage cannot satisfy the control.

Fixture mode intentionally covers only deterministic public sources.
Live mode adds provider-owned `/models` sources across the hosted
provider set: Anthropic, OpenAI, Hugging Face Router, Gemini, Mistral,
Cohere, xAI, Together, Groq, Cerebras, DeepSeek, Fireworks, DashScope,
MiniMax, Z.AI, Moonshot, Baseten, DeepInfra, SambaNova, NVIDIA NIM,
Nebius Token Factory, FlexAI, Hyperbolic, SiliconFlow, Parasail, and
Atlas Cloud. For NVIDIA, set `NVIDIA_API_KEY`; set `NVIDIA_NIM_BASE_URL`
only when you need a self-hosted NIM or gateway URL. The built-in NVIDIA
default is `https://integrate.api.nvidia.com/v1`.

## Reasoning-effort ladders

`reasoning_effort_levels` is the one capability row nothing else verifies.
Tool formats have `provider tool-probe`, dispatch has `provider
dispatch-audit`, pricing has the rate card — but effort ladders were written by
hand into capability fragments and re-asserted by hand in unit tests, so a wrong
rung stayed invisible until a caller took a provider error in production. Harn
shipped `glm-5.3` declaring a `medium` rung that Z.AI rejects with HTTP 400
until a probe caught it.

`harn provider effort-probe` closes that loop by sending one small live call per
rung and reporting both drift directions:

```bash
harn provider effort-probe --model glm-5.3 --suggest-fragment
harn provider effort-probe --all-declared --one-per-claim --plan
harn provider effort-probe --all-declared --one-per-claim --fail-on-drift --json
```

Every probed rung is a paid request and `--all-declared` selects every route
that declares a ladder, so `--plan` lists the routes and the resulting call
count without calling anything.

Catalog ladders are rules with a `model_match` pattern, so one claim can back
dozens of routes and probing all of them re-measures the same rule and bills for
it each time. `--one-per-claim` probes one representative per distinct
(provider, ladder) claim, which covers the whole claim space for a fraction of
the calls. Each result carries `covers`, the routes its verdict stands for, so a
narrowed run never reads as though it had probed everything.

- `declared_but_rejected` — the catalog promises a rung the route refuses.
  Callers asking for it take a provider error. This is the dangerous direction.
- `accepted_but_undeclared` — the route serves a rung the catalog omits. Harn's
  ladder snap silently redirects callers away from a working rung, so nothing
  breaks but the route is quietly less capable than it is.

The probe stands down every catalog-derived rewrite of the requested effort,
not just the declared-ladder check. Each dialect used to grow its own
`|| probe_ungated` hedge around one omit site; leaving any one of them on
lets the catalog confirm itself. The dropped-parameter case is the worst:
the request succeeds without the rung reaching the wire and the probe
records an acceptance the route never gave. That is how `none` on
OpenRouter GLM-5.3 came back accepted — the top-level field was ungated,
the nested `enabled: false` skip was not. One policy
(`catalog_may_shape_requested_reasoning`) is consulted at every omit
across OpenAI-compatible, Gemini, and Anthropic, so that class of lie is
unrepresentable. Pass `--gated` to keep the checks on for a confirm-only
run; rungs they block are reported as `gated_locally` rather than counted
as provider verdicts, so a gated run never invents drift it did not observe.

`accepted` means the route served the request Harn built for that rung, not that
it honoured the rung. Three limits follow, and the probe is explicit about each
rather than hiding them:

- An OpenAI-compatible endpoint that ignores unknown parameters answers 200 for
  every rung it has never heard of. Each accepted attempt records
  `output_tokens` and `reasoning_tokens` when the provider reports them; usage
  that does not move across three or more accepted rungs is reported on the
  route as `usage_unmoved` rather than left for a human to notice. That is a
  warning, not drift: a swallowed parameter is not a catalog contradiction.
- On a provider where Harn *translates* effort instead of sending it verbatim,
  acceptance describes Harn's projection rather than a provider ladder. Gemini
  maps effort to a thinking budget and resolves `high`, `xhigh`, and `max` to
  the same budget, so all three are served and none of them distinguishes a
  provider-side level. Runs that touch such a route print a note saying so.
- An empty `reasoning_effort_levels` means "unknown/all", not "nothing allowed".
  Harn snaps no caller and refuses no rung for such a route, so an accepted rung
  contradicts nothing and is reported as a measurement rather than as drift.

A served-but-empty completion counts as accepted. A reasoning route spends the
response cap on reasoning first, so a cap tight enough to leave no visible
content makes every accepted rung come back as `empty_generation` — a fact about
`--max-tokens`, not about whether the rung is valid. Reading it as a refusal
produced ladders with holes in the middle that no provider implements.

`--suggest-fragment` prints a corrected `reasoning_effort_levels` row for each
drifting route. Like the tool-probe campaign, it is review-only: the probe
never edits catalog TOML.

Effort ladders are route-specific, not model-specific. The same weights served
through two providers can expose different rungs: Z.AI's `glm-5.3` accepts
`low`/`high`/`max`, while the OpenRouter mirror of the same model accepts
`minimal` through `max` because OpenRouter normalizes effort itself. Both
refuse `none` — GLM-5.3 always reasons. Probe each route you catalog rather
than copying a ladder across a mirror.

### One claim, not three

A fragment can say "this route takes an effort ladder" three ways:
`thinking_modes` containing `effort`, the legacy `reasoning_effort_supported`
flag, and a non-empty `reasoning_effort_levels`. `thinking_modes` owns the
answer, and the other two are absorbed into it — each can only ADD `effort`,
never remove it. Setting `reasoning_effort_supported = false` is therefore not
a way to remove an `effort` mode the row declares; leave `effort` out of
`thinking_modes` and make no claim elsewhere.

Absorbing rather than comparing makes the contradiction unrepresentable instead
of merely detectable, which is why there is no separate lint for it. Nineteen
shipped routes had been contradicting themselves, and the failure was invisible
because both halves of Harn read different fields: the reasoning policy read the
flag and produced an effort request, and the option validator read the modes and
refused the request the policy had just built — with the same message a route
that genuinely has no effort control produces.

The one residual invariant, that a declared ladder implies the `effort` mode, is
asserted for every catalogued route by
`effort_capability_agrees_across_every_catalogued_route`.

Prefer the canonical spelling in new fragments:

```toml
thinking_modes = ["enabled", "effort"]
reasoning_effort_levels = ["low", "high", "max"]  # measured, not asserted
```

## Catalog source and generated artifacts

Harn authors edit small TOML fragments under
`crates/harn-vm/src/llm/catalog_sources/`. Harn generates the embedded
runtime snapshot at `crates/harn-vm/src/llm/providers.toml` from those
fragments. Provider capability rules use the same pattern:
`crates/harn-vm/src/llm/capability_sources/` generates
`crates/harn-vm/src/llm/capabilities.toml`.

```bash
# Regenerate providers.toml, capabilities.toml, and all checked-in
# provider catalog artifacts from source fragments in one hermetic pass.
harn provider catalog generate

# CI gate: fail if any generated provider catalog artifact drifted.
harn provider catalog generate --check
```

Direct edits to `crates/harn-vm/src/llm/providers.toml` and
`crates/harn-vm/src/llm/capabilities.toml` are invalid. The files are
checked in so Harn can still embed known-good offline snapshots with
`include_str!`, but `make check-provider-catalog` proves every checked-in
provider catalog projection matches the fragments.

One capability rule may cover exact aliases without copying its body:

```toml
[[provider.gemini]]
model_match = ["gemini-3.7*", "models/gemini-3.7*"]
native_tools = true
```

The list is one ordered rule. Keep separate rows when precedence or capability
values differ. Model routes stay separate across providers because their
prices, wire IDs, quotas, and availability can change independently.

Harn also checks in generated artifacts under `spec/provider-catalog/`
so downstream hosts do not need to parse Harn internals:

- `provider-catalog.json` — normalized providers, models, aliases,
  variants, QC defaults, capabilities, pricing, family/lineage
  metadata, reviewer-diversity hints, deprecation metadata (including
  the structured `superseded_by` migration pointer), fast-mode tier
  metadata (the accelerated-serving opt-in knob, its premium pricing,
  and lifecycle — described but off by default), serving performance
  observations such as TTFT/output rate/time-to-answer with source and
  verification date, serverless-vs-dedicated availability,
  endpoint/auth/extra-header metadata, provider
  healthcheck probes, and provider caveats;
- `provider-catalog.schema.json` — JSON Schema for the catalog
  contract;
- `harn-provider-catalog.d.ts` — type-only TypeScript declarations for
  hosts that load `provider-catalog.json` directly;
- `HarnProviderCatalog.swift` — Swift `Codable` types for hosts that
  load `provider-catalog.json` directly.

Use the `providers` command group for the artifact lifecycle:

```bash
# Regenerate the embedded TOML snapshot and all checked-in artifacts.
harn provider catalog generate

# Validate logical catalog invariants, JSON Schema compatibility, and
# checked-in artifact drift.
harn provider catalog generate --check

# Run the existing refresh workflow through the command group.
harn provider catalog refresh --check

# Regenerate the checked-in capability matrix docs.
harn provider catalog matrix

# CI gate: compare the capability matrix docs with capabilities.toml.
harn provider catalog matrix --check
```

Matrix generation is hermetic by default. To inspect local coding-agent parity
receipts without changing checked-in output, pass them explicitly:

```bash
harn provider catalog matrix \
  --empirical .harn-runs/coding-agent-bench/latest/tool_mode_parity_overlay.toml \
  --stdout
```

`make gen-provider-catalog` and `make check-provider-catalog` route through the
same source-fragment graph as `harn provider catalog generate`, so stale
embedded snapshots from the running binary cannot influence checked-in output.
The full `make all` gate includes `check-provider-catalog`,
`check-provider-matrix`,
`check-provider-support`, and the refresh workflow drift gate.

For local or private models, pass a providers-style TOML overlay to
`harn provider catalog validate --overlay <path>` or
`harn provider catalog export --overlay <path>`. `export` is for explicit
overlay artifacts only; checked-in catalog files are generated with
`harn provider catalog generate`. Overlays are merged with the
same precedence as runtime provider config, so private providers,
aliases, deprecation notes, quality tags, pricing, and transport
settings can be validated before they are published.

Overlays can also hide baseline routes that are broken or unsupported
in the embedding product. `[suppress]` removes the model row, its
aliases, and any recommendation variant derived from it from the
exported and served artifact (runtime resolution of an explicitly
requested id is unaffected):

```toml
[suppress]
routes = [
  "together:Qwen/Qwen3-Coder-Next-FP8",
  "ollama:qwen3.6:35b-a3b-coding-nvfp4",
]
```

Selectors are `provider:model_id`, split on the first colon only, so
model ids that themselves contain colons (Ollama image tags) work.
Because an overlay's `[models]` entries replace whole rows, pairing a
new row with a `[suppress]` entry for the old id also expresses a route
rename — no post-export patching required.

To adjust a single field of a baseline row without copying the whole row
(and silently freezing its other fields against catalog updates), use a
field-wise `[patch.models.<id>]` patch instead of a `[models.<id>]`
replacement — see "Field-wise catalog patches" in the providers guide:

```toml
[patch.models."deepinfra/openai/gpt-oss-120b"]
stream_timeout = 1200.0
```

`harn provider catalog overlay-audit --overlay <file> --check` finds the
entries that have already drifted into whole-row copies or stopped saying
anything at all. Run it from the embedding product's CI, with the Harn release
that product pins — see "Auditing an overlay" in the providers guide.

Structured capability fields (`tool_support`, `modalities`,
`reasoning`, `prompt_cache`) come from the capability matrix, not from
`models.*.capabilities` tags (legacy, parse-only). For overlay-declared
private or local models, pass a matching capabilities overlay (the same
layout as the built-in `capabilities.toml`):

```bash
harn provider catalog export --overlay providers.toml \
  --capabilities-overlay capabilities.toml --output-dir out
```

```toml
[[provider.private]]
model_match = "*"
native_tools = true
vision = true
prompt_caching = true
```

The serve runtime honors the same data via the manifest
`[capabilities]` section, so the exported artifact and the live
`_harn/providerCatalog` / `GET /v1/provider-catalog` responses agree.

### Model picker presentation metadata

Model-selection UIs should render from `[presentation]` records instead of
grouping model IDs with host-side string rules. Recommendation variants are
stable keyed rows with authored copy and one small selector vocabulary:

```toml
[presentation.variants.balanced]
order = 20
label = "Balanced"
description = "Default cost and capability trade-off for everyday work."
selector = { kind = "alias", name = "mid" }
```

Selectors may be `alias`, `model`, `best_local`, `cheapest_hosted`,
`largest_vision_context`, or `largest_context`. The exported `variants` array
contains only the resolved provider and model, so clients do not implement the
selector logic.

Families describe one- or two-dimensional pickers. Use a `model` dimension
when values select concrete model IDs, and a `reasoning_effort` dimension when
values select provider effort tokens:

```toml
[presentation.families.example]
label = "Example family"
plain_description = "Choose a model size and how much time it spends reasoning."

[[presentation.families.example.dimensions]]
key = "variant"
label = "Model size"
plain_description = "Larger variants trade speed and cost for capability."
kind = "model"
ordered_values = [
  { value = "small", label = "Small", plain_description = "Fast and inexpensive.", relative_cost_hint = 1, relative_speed_hint = 5, model_id = "example-small" },
  { value = "large", label = "Large", plain_description = "Most capable.", relative_cost_hint = 5, relative_speed_hint = 1, model_id = "example-large" },
]

[[presentation.families.example.dimensions]]
key = "effort"
label = "Thinking time"
plain_description = "More thinking can improve difficult answers."
kind = "reasoning_effort"
ordered_values = [
  { value = "low", label = "Low", plain_description = "Brief reasoning.", relative_cost_hint = 1, relative_speed_hint = 5 },
  { value = "high", label = "High", plain_description = "Deeper reasoning.", relative_cost_hint = 4, relative_speed_hint = 2 },
]

[[presentation.families.example.presets]]
id = "balanced"
label = "Balanced"
plain_blurb = "A practical default for everyday work."
coordinates = { variant = "small", effort = "high" }
```

For a one-dimensional effort family, set `model_id` on the family and omit a
`model` dimension. Presentation-family overlays replace a same-ID family
whole; ordered dimensions and presets are not merged element by element.

Every referenced model must belong to one provider. Effort availability is
not duplicated in the presentation row: the artifact projects each model's
capability-owned `reasoning.effort_levels`, and a host enables a grid cell only
when the selected model contains that effort token. Presets must resolve to a
valid cell. Add a one-sentence `blurb` to each concrete model row so the detail
view can explain its trade-off without marketing copy.

The exported `families` array preserves dimension and preset order. The flat
`models` array remains available for scripting and backward-compatible lists.

## Runtime surfaces

Thin clients should prefer the live harn-serve catalog when a runtime is
available:

- REST: `GET /v1/provider-catalog`
- ACP: JSON-RPC request method `_harn/providerCatalog`

Both return the same provider catalog v8 artifact shape:
`schema_version`, `schema`, `generated_by`,
`providers`, `models`, `aliases`, `variants`, `families`, `routing_routes`, and
`qc_defaults`. Each model owns a non-empty `display_name` for compact status
surfaces alongside its full `name` identity. Each provider's
`cache_usage_accounting` distinguishes a verified zero-hit cache report from a
route that does not expose cache usage. The
response is already normalized through the server's effective provider
and capability overlays, so clients can render model pickers, key
requirements, aliases, local/cloud grouping, context windows, tool
support, regional endpoint selectors (`endpoint.region_env` plus
`endpoint.regions`), presentation families/presets, and pricing without
shipping their own model/provider tables.
`routing_routes` is the host-trusted route-decision projection for products that
need provider/model/family/capability/timeout rows without resolving live
secret values.

When harn-serve is not running, clients can still use the checked-in
`spec/provider-catalog/provider-catalog.json` artifact as a bundled
baseline. Product and user overlays should be composed in this order,
with later layers winning per key:

1. Harn's bundled provider catalog (`crates/harn-vm/src/llm/providers.toml`,
   generated from `crates/harn-vm/src/llm/catalog_sources/`, or the
   generated `provider-catalog.json` when the client cannot load TOML
   directly)
2. product or managed `providers.toml` overlay
   (`HARN_HOST_PROVIDERS_CONFIG` for an embedding host)
3. user-global `providers.toml` (`HARN_PROVIDERS_CONFIG` or
   `~/.config/harn/providers.toml`)
4. workspace/package `[llm]` tables for the current run

Once a client has fetched `GET /v1/provider-catalog` or
`_harn/providerCatalog`, it should treat that response as the effective
catalog and avoid applying client-side model/provider patches on top.

## Runtime refresh

Harn can also install a validated runtime overlay on top of the bundled
catalog without rebuilding the binary:

```harn
fn main(harness: Harness) {
  const report = harness.llm.catalog_refresh()
  harness.stdio.println(to_string(report.status))
}
```

The same primitive is available as the free builtin
`llm_catalog_refresh(options?)` for scripts that do not receive a
`Harness`. `options.url` overrides the source URL and `options.force`
ignores a fresh cache entry. The default source is
`https://harnlang.com/provider-catalog/provider-catalog.json`;
set `HARN_PROVIDER_CATALOG_URL` to point at a different catalog.

Refresh behavior is intentionally fail-closed:

- `HARN_DISABLE_CATALOG_REFRESH=1` skips refresh and keeps the bundled
  baseline.
- Remote documents are deserialized against the generated provider
  catalog contract and then checked with the same logical validator used
  by `harn provider catalog validate`.
- Signed envelopes use an Ed25519 signature over the canonical catalog
  JSON. Configure trusted keys with
  `HARN_PROVIDER_CATALOG_TRUSTED_KEYS=key_id=base64_public_key`.
- Unsigned documents are accepted only from loopback development URLs,
  or when `HARN_PROVIDER_CATALOG_ALLOW_UNSIGNED=1` is explicitly set.
- Valid catalogs are cached under
  `$HARN_STATE_DIR/cache/provider-catalog/` with their ETag and TTL.
  Network failures or malformed documents fall back to a valid cached
  catalog when one exists, otherwise to the bundled baseline.
- Refresh is skipped inside a live `agent_loop`; call it before entering
  the loop so model selection stays deterministic for the run.

`harn provider catalog show --refresh` runs the same refresh path before
printing the catalog. ACP model selectors read the merged runtime
catalog, so newly refreshed model IDs appear in clients without
regenerating Swift or TypeScript code.

## Architecture

Three layers, kept deliberately small so other repos can extend or
replace them without forking the entry script.

### Pure logic: `scripts/provider_catalog_refresh.harn`

- `observation(provider, model_id, fields, provenance)` — constructor
  for the per-source observation record. Every observation carries
  the source URL, kind (`html` / `json` / `toml`), owner (`provider`
  / `aggregator`), `observed_at`, `fetched_at`, confidence (`high` /
  `medium` / `low`), `requires_key`, and optional `terms_notes`.
- `normalize_observations(raw)` — reduces overlapping observations
  for the same `(provider, model_id)` into one canonical record.
  Provider-owned sources win on conflicting numeric or capability
  claims; aggregator-owned sources fill gaps. Returns the canonical
  list plus a conflict log surfaced in the report.
- `build_drift(observations, catalog)` — compares observations to a
  catalog dict (live `harness.llm.provider_catalog()` for `--live`, the
  bundled fixture for `--check`) and returns
  `{added, removed, changed, unknown_pricing, low_confidence,
  requires_key}`. Removals only fire when at least one source for
  that provider successfully reported, so a missing API key never
  silently looks like a removal. Missing price fields mean “not reported,”
  not “removed.” Active dated promotions are used for price comparison.
- `render_markdown_report(drift, conflicts, adapter_runs, meta)` —
  renders the section-by-section markdown report (adapter status,
  added/removed/changed models, conflicts, unknown pricing,
  low-confidence claims, key-required observations).
- `render_candidate_toml(observations, meta)` — renders the
  review-ready TOML fragment.

### Source adapters: `scripts/provider_catalog_sources.harn`

Each adapter is a function `adapter(env, config) -> {run, observations}`.

- `html_pricing_table_adapter(env, config)` — extracts a pricing /
  context-window table from an HTML source via the `std/web`
  builtins. `config.column_map` maps HTML header text to observation
  fields, so the adapter handles different page layouts without
  open-coded HTML parsing.
- `json_api_adapter(env, config)` — fetches a JSON endpoint and runs
  a `config.mapper` closure to extract model records.
- `key_required_adapter(env, config)` — wraps another adapter and
  gates it on `config.key_envs`. When no required env var is present,
  the adapter records `status: "skipped"` and lists the gating env
  names in the report instead of pretending the source returned empty.

### Entry script: `scripts/update_provider_catalog.harn`

Wires four canonical adapters (Anthropic and OpenAI pricing pages,
the OpenRouter public `/api/v1/models` index, and a key-gated
Fireworks API stub). Each adapter spec is built by a small
factory function so the manifest stays readable.

## Provenance contract

Each observation carries:

| field | source |
|---|---|
| `provider`, `model_id` | adapter |
| `name`, `context_window`, `pricing`, `capabilities` | adapter |
| `source` / `source_url` | adapter config |
| `source_kind` | adapter (`html` / `json` / `toml`) |
| `source_owner` | adapter (`provider` / `aggregator`) |
| `observed_at` | workflow harness |
| `fetched_at` | `web_fetch` envelope |
| `confidence` | adapter config (`high` / `medium` / `low`) |
| `requires_key` | adapter (true for `key_required_adapter`) |
| `terms_notes` | adapter config |

After `normalize_observations`, the merged record adds a `sources`
list that retains every contributing source so reviewers can see
which page or API claimed which value.

## Scheduling

The workflow runs as an ordinary Harn script. To put it on a weekly
cadence, register it as a cron trigger with the standard
`harn-orchestrator` cadence:

```toml
# harn.toml in a project repo
[[triggers]]
id = "weekly-provider-catalog-refresh"
kind = "cron"
provider = "cron"
schedule = "0 9 * * MON"
timezone = "Etc/UTC"
match = { events = ["cron.tick"] }
handler = "refresh_provider_catalog"
budget = { daily_cost_usd = 0.10, max_concurrent = 1 }
```

The handler shells out to `harn run
scripts/update_provider_catalog.harn -- --live` and posts the
generated markdown report to wherever your team reviews catalog
updates (Slack, Linear, PR description). The workflow never publishes
catalog changes on its own.
