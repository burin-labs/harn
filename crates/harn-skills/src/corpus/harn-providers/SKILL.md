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
- Keep provider tool execution distinct with `provider_tools`.
- Preserve tool-call format negotiation and cost controls.
- Preserve mock-provider determinism.

## Capability routing

- Model capabilities should be data-driven.
- Avoid hardcoding provider quirks in caller code.
- Capability checks should describe both positive and negative support.
- JSON schema support is not the same as native JSON support.
- Keep capability-field names such as `reasoning_effort_supported` distinct
  from the public call option `effort`; capability data may describe a wire
  mechanic that callers never spell directly.
- Vision, PDF, audio, tools, cache, and streaming support vary independently.
- Prompt scaffolding may differ by provider.
- Tool prompting may differ by provider.
- Assistant prefill support may differ by provider.
- Developer-role support may differ by provider.
- When routing changes, update the matrix tests or docs that expose it.

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
- Refresh provider observations live: `harn provider catalog refresh --live`.
- Regenerate catalog artifacts: `harn provider catalog generate`.
- Validate catalog artifacts: `harn provider catalog generate --check`.
- Regenerate capability matrix docs: `harn provider catalog matrix`.
- Validate capability matrix docs: `harn provider catalog matrix --check`.

## Verify

- Provider config: `cargo test -p harn-vm config`.
- LLM behavior: targeted VM provider tests.
- Provider matrix: `harn provider catalog matrix --check`.
- Connector manifests: package validation tests.
- Connector package: `harn connector test . --provider <id>`.
- Mock-provider fixtures: targeted conformance or CLI tests.
- JSON surfaces: `harn --json-schemas --command <command>`.
- Docs snippets: `make check-docs-snippets` when examples change.
- Broad VM changes: `cargo test -p harn-vm`.
- Cross-crate provider changes: `make test`.
