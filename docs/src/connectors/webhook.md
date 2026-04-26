# Generic webhook connector

`GenericWebhookConnector` is the built-in raw HTTP ingress primitive for
generic webhook deliveries. It verifies supported HMAC signature conventions
against the raw request body, normalizes the delivery into a `TriggerEvent`
with the built-in `GenericWebhookPayload` shape, and relies on the
orchestrator listener for route selection, backpressure, and inbox dedupe.

This connector is intentionally provider-neutral:

- route-backed ingestion through `harn orchestrator serve`
- raw-body verification for Standard Webhooks, Stripe-style, and GitHub-style
  signatures
- `TriggerEvent` normalization with header redaction, `raw_body` retention, and
  provider payload preservation
- durable inbox-backed dedupe keyed by the normalized `event.dedupe_key` when
  the trigger manifest opts into `dedupe_key`

## Manifest shape

```toml
[[triggers]]
id = "incoming-webhook"
kind = "webhook"
provider = "webhook"
match = { path = "/hooks/incoming" }
handler = "handlers::on_webhook"
dedupe_key = "event.dedupe_key"
secrets = { signing_secret = "webhook/incoming" }

[triggers.webhook]
signature_scheme = "standard"  # "standard" | "stripe" | "github"
timestamp_tolerance_secs = 300
source = "incoming"
```

`signature_scheme` defaults to `"standard"` when omitted. Standard Webhooks and
Stripe-style signatures default to a 5-minute timestamp tolerance. GitHub-style
signatures are untimestamped and therefore ignore timestamp skew.

## Supported signature conventions

The connector delegates signature checks to
`harn_vm::connectors::verify_hmac_signed(...)`, so it inherits the shared
verification rules from C-01:

- verify against the raw inbound bytes, not a reparsed body
- compare signatures in constant time
- enforce a timestamp window for timestamped schemes
- append signature failures to the `audit.signature_verify` event-log topic

Supported variants:

- Standard Webhooks:
  `webhook-id`, `webhook-timestamp`, `webhook-signature: v1,<base64>`
- Stripe-style:
  `Stripe-Signature: t=<unix>,v1=<hex>[,v1=<hex>...]`
- GitHub-style:
  `X-Hub-Signature-256: sha256=<hex>`

## Normalized event fields

For successful deliveries the connector produces:

- `provider = "webhook"`
- `kind` from `RawInbound.kind`, then `X-GitHub-Event`, then payload `type` /
  `event`, else `"webhook"`
- `dedupe_key` from the provider-native delivery identifier:
  `webhook-id`, Stripe event `id`, or `X-GitHub-Delivery`
- `signature_status = { state: "verified" }`
- `provider_payload = GenericWebhookPayload`

`GenericWebhookPayload.raw` keeps parsed JSON when the body is JSON. When the
payload is not valid JSON, the connector preserves the bytes as:

```json
{
  "raw_base64": "<base64-encoded body>",
  "raw_utf8": "optional utf-8 view"
}
```

`GenericWebhookPayload.source` comes from `X-Webhook-Source` when present, or
from the binding's optional `webhook.source` override.

## Dedupe

If the trigger manifest declares `dedupe_key`, the connector records the
normalized `event.dedupe_key` in the trigger inbox before dispatch. Replays for
the same binding are dropped before handler execution, and the dedupe claim is
durable across orchestrator restarts for the configured retry retention window.

## Activation and listener integration

The connector's `activate()` hook validates the binding config and reserves
unique `match.path` values across active bindings. The orchestrator listener
maps each incoming HTTP request path to the active trigger binding, passes the
original bytes through `RawInbound`, applies connector normalization, and then
appends accepted events to the dispatcher queue.

Direct `normalize_inbound(...)` calls remain useful for tests and embedding.
When more than one binding is active, callers must pass the selected
`binding_id` in `RawInbound.metadata.binding_id` so the connector can resolve
the configured secret and signature variant.

## Notes and follow-up

- Signature failures are audited even when normalization returns an error.
- Production TLS and request-size limits are owned by the orchestrator listener
  and HTTP server layer, not by this connector.
- Provider-specific business logic should live in pure-Harn connector packages
  that compose this raw webhook substrate rather than adding more Rust
  provider-specific branches here.
