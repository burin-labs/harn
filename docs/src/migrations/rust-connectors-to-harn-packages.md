# Migrating Rust provider connectors to pure-Harn packages

The Rust-side GitHub, Slack, Linear, and Notion connectors are deprecated
compatibility shims under the pure-Harn connector pivot tracked by
[#350](https://github.com/burin-labs/harn/issues/350). The core sunset
groundwork tracked by [#446](https://github.com/burin-labs/harn/issues/446)
has landed; new provider business logic ships in pure-Harn connector packages
so that Harn Cloud and self-hosted orchestrators can adopt connector fixes, new
event families, and provider API changes without waiting for a Harn core release.

This guide is the no-downtime migration path for an orchestrator that today
uses one of the Rust-side providers and wants to cut over to the pure-Harn
package equivalent.

The cron, generic webhook, A2A push, and stream connectors stay in Harn core
and are **not** affected by this migration. HMAC verification, raw-body
access, signature header constants, and signing primitives also stay in core
so that pure-Harn connectors do not need to reimplement them.

## What is changing

Each first-party provider has a pure-Harn replacement that exposes the same
event shape through the connector contract v1 surface:

| Provider | Pure-Harn package |
|---|---|
| GitHub | <https://github.com/burin-labs/harn-github-connector> |
| Slack | <https://github.com/burin-labs/harn-slack-connector> |
| Linear | <https://github.com/burin-labs/harn-linear-connector> |
| Notion | <https://github.com/burin-labs/harn-notion-connector> |

Manifests opt into the pure-Harn replacement by declaring a
`[[providers]]` table that points `connector = { harn = "..." }` at the
package's connector module. The orchestrator already prefers a configured
Harn module over the Rust default, so the Rust connector keeps running
unchanged for any provider that does not declare an override.

## Cutover checklist

The cutover is intentionally per-provider so that an orchestrator can soak
one provider on the pure-Harn implementation before moving the rest.

1. **Install the package.** Add the package as a dependency and run
   `harn install --locked`.

   ```sh
   harn add github.com/burin-labs/harn-github-connector@v0.1.0
   harn install --locked
   ```

2. **Run the contract check.** Confirm the package matches the connector
   contract and your supported event families.

   ```sh
   harn connector check . --provider github
   ```

   For Notion, also exercise the poll path:

   ```sh
   harn connector check . --provider notion --run-poll-tick
   ```

3. **Add a `[[providers]]` override.** Tell the orchestrator to load the
   pure-Harn module for this provider.

   ```toml
   [[providers]]
   id = "github"
   connector = { harn = "vendor/harn-github-connector/src/lib.harn" }
   ```

   Leave existing trigger entries unchanged. Triggers with
   `provider = "github"` automatically resolve through the new connector
   once the override is in place.

4. **Run a parity check against your fixtures.** The recommended pattern
   is to feed canonical webhook bodies through both the Rust connector and
   the pure-Harn package and assert the resulting `TriggerEvent`
   `kind` / `dedupe_key` / `provider_payload` shapes match. The connector
   testkit (`docs/src/connectors/testkit.md`) has the primitives needed to
   stage a `RawInbound` and capture the normalized event in tests.

   First-party connector packages run a parity matrix against the Rust
   payload shapes in their own CI. If your handlers depend on a vendor
   field that is not in the parity fixtures, add it to your local
   `[connector_contract]` fixture set before cutover.

5. **Roll out and verify.** Deploy the manifest change. The orchestrator
   logs a one-line confirmation when it loads the Harn module instead of
   the Rust default. `harn doctor` reports `trigger:<id>` as
   `via <provider>` regardless of which connector implementation is in
   use, so existing health checks keep working.

6. **Keep the Rust connector available during the soak.** If the
   pure-Harn package needs a hotfix, remove the `connector = { harn = ... }`
   override and the orchestrator falls back to the Rust connector with no
   downtime. There is no on-disk migration of inbox state, so a fallback is
   safe to perform mid-flight.

## Harn Cloud specifics

Managed Harn Cloud orchestrators load pure-Harn connector packages through
the same `[[providers]]` mechanism documented above. Connector packages are
resolved through the package manager so the cutover is a manifest change,
not a Harn Cloud release.

## What stays in core

The following primitives stay in Harn core and continue to be the only
supported way to express their respective concerns:

- The `cron` connector (`docs/src/connectors/cron.md`).
- The generic `webhook` connector with HMAC verification, including the
  `webhook-signature` / `webhook-timestamp` / `webhook-id` Standard
  Webhooks-style headers (`docs/src/connectors/webhook.md`).
- HMAC verification helpers under `harn_vm::connectors::hmac`, including
  the canonical signature-header constants used by GitHub, Slack, Linear,
  Notion, Stripe, and Standard Webhooks.
- The A2A push connector and the stream connector for queue-shaped
  ingress.
- Raw HTTP request access (`raw_body`, headers) and signing primitives.

Pure-Harn provider connectors compose these primitives — they do not
duplicate them.

## Removal status

The Harn core prerequisites from [#446](https://github.com/burin-labs/harn/issues/446)
are complete:

- The connector contract conformance harness validates pure-Harn replacements
  through the same adapter path Harn Cloud and self-hosted orchestrators use.
- `NormalizeResult` v1, `poll_tick`, hot-path effect policy, transport
  primitives, structured concurrency, and the connector testkit are in core.
- The OAuth / connect CLI and package manager give connector packages a stable
  install + auth path.
- Harn core has parity fixtures for GitHub, Slack, Linear, and Notion that pin
  the Rust-compatible `TriggerEvent` payload shapes used by first-party package
  CI.

The remaining Rust code is intentionally compatibility-only for the deprecation
window. Do not add new provider-specific Rust connector business logic in this
repository; new service connectors should be packages that register with
`connector = { harn = "..." }`. Removing the compatibility shims is a release
coordination decision, not a prerequisite for new connector development.
