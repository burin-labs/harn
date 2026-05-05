# Migrating Rust provider connectors to pure-Harn packages

GitHub, Slack, Linear, and Notion provider business logic now ships in
pure-Harn connector packages. Harn core keeps only the shared connector
primitives, so Harn Cloud and self-hosted orchestrators can adopt connector
fixes, new event families, and provider API changes without waiting for a Harn
core release.

This guide is the migration path for an orchestrator that still has an old
manifest or deployment model from before the pure-Harn package cutover.

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
package's connector module. Provider-specific Rust fallbacks are no longer
registered for GitHub, Slack, Linear, or Notion, so the override is the
canonical path for those providers.

## Cutover checklist

The cutover is intentionally per-provider so that an orchestrator can soak
one provider on the pure-Harn implementation before moving the rest.

1. **Install the package.** Add the package as a dependency and run
   `harn install --locked`.

   ```sh
   harn add github.com/burin-labs/harn-github-connector@v0.2.0
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

4. **Run a fixture check.** Feed canonical webhook bodies through the
   pure-Harn package and assert the resulting `TriggerEvent`
   `kind` / `dedupe_key` / `provider_payload` shapes match the payloads your
   handlers expect. The connector testkit
   (`docs/src/connectors/testkit.md`) has the primitives needed to stage a
   `RawInbound` and capture the normalized event in tests.

   First-party connector packages run a parity matrix against the Rust
   payload shapes in their own CI. If your handlers depend on a vendor
   field that is not in the parity fixtures, add it to your local
   `[connector_contract]` fixture set before cutover.

5. **Roll out and verify.** Deploy the manifest change. The orchestrator
   logs a one-line confirmation when it loads the Harn module. `harn doctor`
   reports `trigger:<id>` as `via <provider>`, so existing health checks keep
   working.

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

The Harn core prerequisites are complete:

- The connector contract conformance harness validates pure-Harn replacements
  through the same adapter path Harn Cloud and self-hosted orchestrators use.
- `NormalizeResult` v1, `poll_tick`, hot-path effect policy, transport
  primitives, structured concurrency, and the connector testkit are in core.
- The OAuth / connect CLI and package manager give connector packages a stable
  install + auth path.
- First-party package CI owns parity fixtures for GitHub, Slack, Linear, and
  Notion so provider payload behavior can evolve with package releases.

Do not add provider-specific Rust connector business logic in this repository;
service connectors should be packages that register with
`connector = { harn = "..." }`.
