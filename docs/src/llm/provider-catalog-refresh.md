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

Static catalog pricing should reflect the provider's durable rate card,
not launch promotions or short discount windows returned by aggregator
APIs. When a provider publishes time-limited promotional rates, keep the
normal post-promotion rate in the catalog source fragments and capture
the promotion only in human review notes unless the catalog schema grows
an explicit promotion-period field.

The markdown report includes aggregator discoveries for awareness, but the
candidate TOML contains only provider-owned, high-confidence changes and
additions with actionable pricing, context-window, or capability metadata. This
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

Fixture mode intentionally covers only deterministic public sources.
Live mode adds provider-owned `/models` sources across the hosted
provider set: Anthropic, OpenAI, Hugging Face Router, Gemini, Mistral,
Cohere, xAI, Together, Groq, Cerebras, DeepSeek, Fireworks, DashScope,
MiniMax, Z.AI, Moonshot, Baseten, DeepInfra, SambaNova, NVIDIA NIM,
Nebius Token Factory, FlexAI, Hyperbolic, SiliconFlow, Parasail, and
Atlas Cloud. For NVIDIA, set `NVIDIA_API_KEY`; set `NVIDIA_NIM_BASE_URL`
only when you need a self-hosted NIM or gateway URL. The built-in NVIDIA
default is `https://integrate.api.nvidia.com/v1`.

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

Both return the same provider catalog v6 artifact shape:
`schema_version`, `schema`, `generated_by`,
`providers`, `models`, `aliases`, `variants`, `families`, `routing_routes`, and
`qc_defaults`. The
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
  catalog dict (live `llm_provider_catalog()` for `--live`, the
  bundled fixture for `--check`) and returns
  `{added, removed, changed, unknown_pricing, low_confidence,
  requires_key}`. Removals only fire when at least one source for
  that provider successfully reported, so a missing API key never
  silently looks like a removal.
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
