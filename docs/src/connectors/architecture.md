# Connector architecture status

The original Rust-side connector library plan covered shared connector traits,
generic webhooks, cron, GitHub, Slack, Linear, Notion, OAuth helpers, catalog
docs, and provider-specific runtime behavior. That plan has since been split
into a smaller core substrate plus pure-Harn provider packages.

This page is the current source of truth for what belongs in this repository
after the connector pivot.

## Core responsibilities

Harn core owns the primitives that every connector implementation needs:

- the `Connector` trait, registry, provider catalog, and Harn module adapter
- raw HTTP ingress into `RawInbound`, including original body bytes and headers
- `TriggerEvent` normalization, raw-body exposure, dispatcher handoff, and
  inbox/outbox dedupe
- signature helpers for raw-body HMAC verification, constant-time comparison,
  timestamp-window replay checks, and audit events
- the `cron`, generic `webhook`, `a2a-push`, and stream ingress providers
- `NormalizeResult` v1 for ack-first webhooks and batched deliveries
- `poll_tick` scheduling for Harn connector packages
- connector hot-path effect policy for deterministic `normalize_inbound`
  exports
- shared HTTP, encoding, OAuth/connect, package-manager, and testkit surfaces
  that provider packages compose

Provider-specific business logic should not be added to Harn core unless the
ticket is explicitly about compatibility or removal of an existing Rust shim.

## External provider packages

Provider business logic now lives in first-party or community Harn packages.
The first-party package track is:

| Provider | Package repo | Status |
|---|---|---|
| GitHub | <https://github.com/burin-labs/harn-github-connector> | First-party package track |
| Slack | <https://github.com/burin-labs/harn-slack-connector> | First-party package track |
| Linear | <https://github.com/burin-labs/harn-linear-connector> | First-party package track |
| Notion | <https://github.com/burin-labs/harn-notion-connector> | First-party package track |
| GitLab | <https://github.com/burin-labs/harn-gitlab-connector> | Additional forge package track |

Each package should declare connector contract v1 metadata, ship deterministic
fixtures, and pass:

```sh
harn connector check .
```

Poll-based packages should also run:

```sh
harn connector check . --run-poll-tick
```

## Rust compatibility shims

The in-repo Rust GitHub, Slack, Linear, and Notion connectors are compatibility
shims during the pure-Harn package soak. Their sunset and removal use the same
migration path as the pure-Harn package rollout.

For new provider work, use:

- the pure-Harn connector package track for provider packages
- the Rust-provider deprecation path for shim removal
- The external provider package repos for provider-specific event support,
  outbound methods, scopes, fixtures, and release readiness.
- the additional forge package track for GitLab and similar providers

## Core closure checklist

The repository-local connector substrate is considered complete when these
surfaces exist and are tested:

| Surface | Current home |
|---|---|
| Connector trait, registry, and provider metadata | `crates/harn-vm/src/connectors/mod.rs`, `std/triggers::list_providers()` |
| Raw webhook substrate and signed generic webhook receiver | `crates/harn-vm/src/connectors/webhook/`, `crates/harn-cli/src/commands/orchestrator/listener.rs` |
| Cron scheduler primitive | `crates/harn-vm/src/connectors/cron/` |
| Raw body, bytes, HMAC, encoding, and constant-time helpers | `TriggerEvent.raw_body`, stdlib crypto/encoding builtins, `connectors::hmac` |
| Durable inbox dedupe and dispatcher handoff | `crates/harn-vm/src/triggers/inbox.rs`, `triggers/dispatcher/` |
| Rate-limit and `Retry-After` behavior | connector clients plus shared HTTP retry/backoff builtins |
| Harn connector contract, `NormalizeResult`, `poll_tick`, and effect policy | `crates/harn-vm/src/connectors/harn_module.rs`, `crates/harn-lint/src/tests/connector_effect_policy.rs` |
| Connector package conformance harness | `harn connector check` and connector contract fixtures |
| Catalog, examples, and migration guidance | `docs/src/connectors/catalog.md`, `examples/triggers/`, `docs/src/migrations/rust-connectors-to-harn-packages.md` |

Future work should update those newer ownership surfaces, not reopen the old
Rust-provider plugin-library plan.
