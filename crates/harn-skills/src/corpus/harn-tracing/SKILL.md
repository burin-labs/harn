---
name: harn-tracing
short: Transcripts, receipts, traces, replay, and observability surfaces.
description: Use for Harn tracing, transcript lifecycle, run records, receipts, replay, and observability workflows.
when_to_use: Use when inspecting or changing run records, transcripts, replay paths, receipts, telemetry, or portal activity data.
---

# Harn tracing

Use this skill when inspecting or changing transcripts, receipts, run records,
replay, observability events, or portal-facing activity data.

Pair it with [[harn-orchestration]] for workflow events and [[harn-testing]] for deterministic replay fixtures.

## Start here

- Transcript code is primarily under `crates/harn-vm/src/orchestration/`.
- Run-record code lives near orchestration record modules.
- Replay surfaces feed CLI audits, evals, and portal views.
- Receipts describe durable actions and decisions.
- OTel and Langfuse exporters are observability surfaces, not core logic.
- The portal UI lives under `crates/harn-cli/portal/`.
- Only edit portal code when the stored activity contract changes.
- Treat persisted trace data as a compatibility contract.

## Transcript rules

- Preserve event ordering.
- Preserve event ids.
- Preserve cursors.
- Preserve session ids.
- Preserve run ids.
- Preserve worker lineage.
- Preserve parent-child relationships.
- Preserve private/public transcript boundaries.
- Do not leak secrets into public event channels.
- Normalize old records rather than breaking replay.

## Receipt rules

- Keep receipts machine-readable.
- Keep action ids stable.
- Keep provenance fields explicit.
- Include enough context for audit.
- Avoid full private prompt content unless the receipt channel is private.
- Record approvals and denials distinctly.
- Record skipped work when it affects outcome.
- Record replay-relevant hashes when available.
- Human summaries may change more freely than JSON contracts.
- Use [[harn-diagnostics]] for receipt validation failures.

## Replay rules

- Replay should not call live providers.
- Replay should not require network access.
- Replay should not depend on wall-clock time.
- Replay should preserve deterministic ordering.
- Replay fixtures should be small and explain the scenario.
- Replay failures should point to the first mismatched event.
- Cached inputs should be explicit.
- Signed timestamps should stay verifiable.
- Missing optional fields should have deterministic defaults.
- Add migration coverage for persisted schema changes.

## Observability rules

- Spans should map to real workflow phases.
- Span names should be stable.
- Span attributes should avoid secrets.
- Links should connect resume and handoff relationships.
- Metrics should be low-cardinality.
- Logs should be useful without full prompts.
- OTel changes should not change runtime semantics.
- Langfuse exports should be derived from canonical events.
- Portal data should be derived from persisted records.
- Backfills should be explicit and testable.

## Common mistakes

- Do not treat rendered text as the source of truth.
- Do not add a second transcript schema.
- Do not drop unknown fields during read-write paths.
- Do not store only local filesystem paths in portable records.
- Do not make replay depend on current provider config.
- Do not conflate stdout, stderr, and transcript events.
- Do not merge private and public prompt content.
- Do not assume event delivery is immediate.
- Do not rely on sleeps in observability tests.
- Do not hand-edit generated run artifacts.

## Verify

- Run-record tests: targeted `cargo test -p harn-vm record`.
- Replay tests: targeted `cargo test -p harn-vm replay`.
- Orchestration events: `cargo test -p harn-vm orchestration`.
- CLI audit or eval: `cargo test -p harn-cli --test <test-name>`.
- Portal contract changes: `npm run portal:lint`.
- Portal tests: `npm run portal:test`.
- Portal build: `npm run portal:build`.
- Trace fixture changes: targeted replay or eval fixture tests.
- Flake guard: `make lint-test-patterns` when adding tests.
- Broad shared changes: `make test`.
