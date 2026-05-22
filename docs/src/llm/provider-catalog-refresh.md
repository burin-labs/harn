# Provider catalog refresh workflow

`scripts/update_provider_catalog.harn` is the Harn-native workflow that
periodically collects model availability, pricing, and capability
signals from provider sources, normalizes them, and emits:

- a markdown **drift report** under
  `.harn-runs/provider_catalog/drift-report.md`;
- a TOML **candidate patch** under
  `.harn-runs/provider_catalog/candidate.toml`.

The workflow never mutates the shipped catalog. The patch is a review
aid: diff it against `crates/harn-vm/src/llm_config.rs` or your
project's `harn.toml` overlay before landing changes.

## Running the workflow

```bash
# Default: replay against bundled fixtures, write report + candidate
# under .harn-runs/provider_catalog/.
harn run scripts/update_provider_catalog.harn

# Live mode: hit real provider sources. Key-required adapters read
# their secrets from env vars (e.g. FIREWORKS_API_KEY); missing keys
# produce a "skipped" diagnostic in the report instead of a failure.
harn run scripts/update_provider_catalog.harn -- --live

# CI gate: same fixture replay, but compare against the committed
# goldens at scripts/provider_catalog_fixtures/expected_*.
harn run scripts/update_provider_catalog.harn -- --check

# Refresh the committed goldens after intentional adapter changes.
harn run scripts/update_provider_catalog.harn -- --check --update
```

The CI gate is wired into `make check-provider-catalog-drift` and runs
from `make all`.

## Generated catalog artifacts

The runtime provider config remains the source of truth, but Harn also
checks in generated artifacts under `spec/provider-catalog/` so
downstream hosts do not need to parse Harn internals:

- `provider-catalog.json` — normalized providers, models, aliases,
  variants, QC defaults, capabilities, pricing, deprecation metadata,
  serverless-vs-dedicated availability, endpoint/auth metadata, and
  provider caveats;
- `provider-catalog.schema.json` — JSON Schema for the catalog
  contract;
- `harn-provider-catalog.ts` — TypeScript types plus compatibility
  helpers such as `MODEL_CATALOG`, `ALIASES`, `QC_DEFAULTS`,
  `entryFor`, and `pricingFor`;
- `HarnProviderCatalog.swift` — Swift `Codable` types plus the
  embedded catalog JSON string.

Use the `providers` command group for the artifact lifecycle:

```bash
# Regenerate all checked-in artifacts.
harn providers export

# Validate logical catalog invariants, JSON Schema compatibility, and
# checked-in artifact drift.
harn providers validate --check-artifacts

# Run the existing refresh workflow through the command group.
harn providers refresh --check
```

`make gen-provider-catalog` runs `harn providers export`, and
`make check-provider-catalog` runs
`harn providers validate --check-artifacts`. The full `make all` gate
includes both `check-provider-catalog` and the refresh workflow drift
gate.

For local or private models, pass a providers-style TOML overlay to
`harn providers validate --overlay <path>` or
`harn providers export --overlay <path>`. Overlays are merged with the
same precedence as runtime provider config, so private providers,
aliases, deprecation notes, quality tags, pricing, and transport
settings can be validated before they are published.

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
