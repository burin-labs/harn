# Generic webhook connector

`GenericWebhookConnector` is the first concrete inbound connector built on top
of the C-01 `Connector` trait. It accepts generic HTTP webhook deliveries,
verifies supported HMAC signature conventions against the raw request body, and
normalizes the delivery into a `TriggerEvent` with the built-in
`GenericWebhookPayload` shape.

The current implementation is intentionally small:

- activation-only; the O-02 HTTP listener still wires request routing later
- raw-body verification for Standard Webhooks, Stripe-style, and GitHub-style
  signatures
- `TriggerEvent` normalization with header redaction and provider payload
  preservation
- process-local dedupe stub keyed by the manifest `dedupe_key` opt-in until the
  durable trigger inbox lands

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
normalized `event.dedupe_key` in the current inbox dedupe stub and rejects
replays for the same binding. This is process-local today; durable inbox-backed
dedupe is still deferred to T-09.

## Activation and listener integration

The connector's `activate()` hook validates the binding config and reserves
unique `match.path` values across active bindings. Because O-02 is still
outstanding, request routing is not implemented here. Until the listener lands:

- a single active binding can call `normalize_inbound(...)` directly
- multiple active bindings must pass the selected `binding_id` in
  `RawInbound.metadata.binding_id`

## Notes and follow-up

- Signature failures are audited even when normalization returns an error.
- Production TLS handling is owned by the eventual listener, not this connector.
- Streaming request bodies larger than 10 MiB is still a follow-up item.
