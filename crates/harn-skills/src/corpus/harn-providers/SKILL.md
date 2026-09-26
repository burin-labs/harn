---
name: harn-providers
short: LLM provider configuration, model routing, and provider capability behavior.
description: Use for Harn provider setup, model routing, provider capability matrices, and llm_call options.
when_to_use: Use when wiring or debugging LLM providers, model routes, provider readiness, or structured-output capabilities.
---

# Harn providers

Use this skill when wiring or debugging LLM providers, model routes, structured
output, connector providers, or provider readiness.

Pair it with [[harn-orchestration]] for workflow behavior and [[harn-testing]] for deterministic provider fixtures.

## Start here

- `docs/llm/harn-quickref.md` documents `llm_call` and `llm_stream_call`.
- The quickref also covers `provider: "auto"`, schemas, and retries.
- `docs/llm/harn-triggers-quickref.md` documents connector provider manifests.
- LLM configuration and routing live under `crates/harn-vm/src/`.
- Package/provider manifest handling lives under `crates/harn-cli/src/package/`.
- Provider capability rows are surfaced by CLI matrix commands.
- Mock providers are the default for deterministic tests.
- Never require live credentials in ordinary CI.

## `llm_call` options

- Keep `provider` explicit when behavior depends on a vendor.
- Use `provider: "auto"` only when capability-based routing is acceptable.
- Keep `model` optional only when routing policy can choose safely.
- Express structured output through `output`: `"json"`, a schema, or
  `{schema, strict?, validation?, stream_abort?}`.
- Preserve `schema_retries` and `repair` behavior.
- Compose system text with the single `system` string-or-fragment-list option.
- Use `effort` and `speed` for provider-neutral intent.
- Use `timeout_ms` and `idle_timeout_ms`; both are milliseconds.
- Put wire-specific fields below `provider_options.<provider>`.
- Treat portable generation options as required caller intent. An authored
  capability denial is a terminal `invalid_request`; adapters must not drop it.
- Unknown custom generation routes remain open-world. Explicit `cache: true`
  and `prompt_cache_ttl` require authored support because cache lowering is
  provider-specific; TTL values must be listed in `prompt_cache_ttls`.
- Let `cache_breakpoint_style` choose the request marker location. Provider
  adapters must use the canonical lowering and preserve explicit caller markers.
- Keep provider tool execution distinct with `provider_tools`.
- Preserve tool-call format negotiation and cost controls.
- Preserve mock-provider determinism.

## Capability routing

- Model capabilities should be data-driven.
- Avoid hardcoding provider quirks in caller code.
- Resolve `message_wire_format` and `live_endpoint_family` as one dialect for
  request, stream, response, and error handling. Do not choose a builder or
  parser independently from provider strings or response headers.
- Put route-specific reasoning behavior in capability rows and resolve it with
  `harness.llm.apply_reasoning_policy`; do not branch on model IDs, providers,
  families, or lineages in stdlib policy.
- Put reusable ordered routes in catalog `[model_ladders.<name>]` rows. Use
  `ladder: "<name>"` for calls and `harness.llm.model_ladder(name)` when policy
  or tooling needs to inspect the steps.
- Capability checks should describe both positive and negative support.
- JSON schema support is not the same as native JSON support.
- Keep capability-field names such as `reasoning_effort_supported` distinct
  from the public call option `effort`; capability data may describe a wire
  mechanic that callers never spell directly.
- Keep provider continuation metadata private and byte-exact. The capability
  field `reasoning_round_trip` defaults to `strip`; use `echo_signed` only for
  signed blocks and `echo_same_key` only with a typed history wire field.
- Vision, PDF, audio, tools, cache, and streaming support vary independently.
- Prompt scaffolding may differ by provider.
- Tool prompting may differ by provider.
- Assistant prefill support may differ by provider.
- Developer-role support may differ by provider.
- When routing changes, update the matrix tests or docs that expose it.
- Check every routed attempt after its link-specific model and option overrides
  resolve; admission belongs before rate limiting and provider transport.

## Connector providers

- Connector packages should declare provider ids.
- Connector packages should declare supported event kinds.
- Connector packages should declare payload schemas.
- Inbound normalization should be deterministic.
- Optional polling should be explicit.
- Connector `call` behavior should be capability-gated.
- Keep OAuth and secret handling out of docs and fixtures.
- Do not embed tokens in package manifests.
- Use package validation tests for manifest shape.
- Use trigger quickref examples for user-facing behavior.

## Cost and reliability

- Cost ceilings should be caller-visible.
- Retry behavior should be bounded.
- Provider fallback should be explicit and auditable.
- Do not silently change the chosen provider after a partial tool exchange.
- Record enough provider metadata for debugging.
- Avoid logging secrets or raw private prompts in public traces.
- Keep failure messages actionable.
- Map provider failures to stable diagnostics where possible.
- Preserve transcript shape when provider calls are replayed.
- Use [[harn-tracing]] for transcript and receipt implications.

## Review checklist

- Does this change affect `provider: "auto"`?
- Does this change structured-output behavior?
- Does this affect streaming?
- Does this affect tool calls?
- Does this affect provider catalogs?
- Does this affect connector manifests?
- Does this affect offline behavior?
- Does this require docs or CLI help updates?
- Does this need conformance or mock-provider fixtures?
- Does this remain deterministic without network access?

## Catalog and matrix commands

- Refresh provider observations with fixtures: `harn provider catalog refresh --check`.
- Refresh provider observations live: `harn provider catalog refresh --live --json`.
- Regenerate catalog artifacts: `harn provider catalog generate`.
- Validate catalog artifacts: `harn provider catalog generate --check`.
- Regenerate capability matrix docs: `harn provider catalog matrix`.
- Validate capability matrix docs: `harn provider catalog matrix --check`.

## Maintain the live catalog

1. Run the exact-source Harn CLI with authorized provider keys in its process
   environment. Never put key values in arguments, source, reports, or a remote
   file. Read the JSON coverage summary from `refresh --live --json` and the
   full `.harn-runs/provider_catalog/refresh.json` evidence. A partial run
   names skipped, empty, and failed adapters; zero observations is unmeasured, not a
   clean catalog.
2. Review the typed drift and source provenance. A model-index omission is a
   retirement lead, not proof of deprecation. Verify identity and successor
   against first-party release notes or the provider's current rate card.
   Use provider list prices for the actual route; do not copy an aggregator's
   floating cheapest-host price into a direct-provider row.
   For a trusted release or pricing notice, run
   `harn run scripts/provider_catalog_notice.harn -- --notice <json> --provider <provider> --model <model>`
   with a configured own-key route. Harn's schema-constrained model extraction
   produces a reviewable receipt and refuses ambiguous identities. A new model
   becomes an incomplete proposal, with the original extraction in the receipt.
   Keep `--apply` off until the candidate is independently verified.
3. For each new chat route with available credentials, run
   `harn provider tool-probe <provider> --model <id> --mode non-streaming --json true`.
   Require `classification = structured_native_tool_call` before advertising
   native tool use and record `usage.cost_usd`. Keep an inaccessible route
   explicitly unverified instead of treating an aggregator mirror as proof of
   its direct adapter.
4. Change the owning catalog fragments. Record `deprecated` and
   `superseded_by`, preserve aliases, and move defaults off deprecated rows.
   Pin curated support recommendations before generating projections. Check
   downstream Burin overlays after the next Harn repin; Burin supplies its
   credentials and product-specific aliases, while Harn owns route semantics.
5. Regenerate the catalog, matrix, and support projections, run their checks
   and the repository audit gate, then open a reviewable PR. Put measured
   coverage, probe costs, skipped adapters, and unverified claims in its
   summary. Never treat a file's existence or a green inventory count as proof
   that a route served a tool call.

## Verify

- Provider config: targeted `make test-one` checks in `harn-vm`.
- LLM behavior: targeted VM provider tests.
- Provider matrix: `harn provider catalog matrix --check`.
- Connector manifests: package validation tests.
- Connector package: `harn package verify . --provider <id>`.
- Mock-provider fixtures: targeted conformance or CLI tests.
- JSON surfaces: `harn --json-schemas --command <command>`.
- Docs snippets: `make check-docs-snippets` when examples change.
- Broad VM changes: `make test ARGS='-p harn-vm'`.
- Cross-crate provider changes: `make test`.
