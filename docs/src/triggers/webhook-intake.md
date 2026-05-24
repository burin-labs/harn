# Generic webhook intake substrate

The webhook intake substrate is the lowest-level layer connectors compose
with to absorb webhook deliveries. It is deliberately ignorant of any
specific provider (GitHub, GitLab, Linear, Slack, Stripe, ...). A
connector declares:

- a path scope (e.g. `/hooks/github`)
- a signature header + algorithm + format the substrate verifies
- a delivery-id header the substrate dedupes on
- a topic to republish accepted deliveries onto

and the substrate handles the rest: HMAC verification, delivery-id
deduplication (durable across process restarts via the trigger inbox),
and republishing onto the chosen event-log topic. Per-forge event
normalization lives in the connector that consumes the topic.

The intake substrate sits **below** the higher-level `GenericWebhookConnector`
(see [Generic webhook connector](../connectors/webhook.md)). The connector
normalizes payloads into `TriggerEvent` shapes; the substrate just gets
opaque bytes onto a topic safely. New per-forge connectors are encouraged
to compose the substrate directly rather than copy-paste signature /
dedupe logic.

## Surface

The substrate is exposed as five Harn builtins.

### `webhook_intake_register(config) -> dict`

Register an intake. Returns a snapshot dict
(`{ id, path, topic, signature_header, signature_prefix,
signature_encoding, algorithm, allow_legacy_sha1, delivery_id_header,
dedupe_ttl_seconds }`). Config keys:

| Key | Required | Default | Description |
|---|:---:|---|---|
| `id` | no | generated `intake_<uuid>` | Pin the intake id; needed for dedupe to survive process restart. |
| `path` | no | none | HTTP path scope. When set, `webhook_intake_feed` rejects deliveries on a different path. |
| `secret` | yes | — | HMAC key. Accepts a string or a `bytes` value (e.g. from `secret_get`). |
| `signature_header` | yes | — | Header name carrying the signature, e.g. `"x-hub-signature-256"`. |
| `signature_prefix` | no | `"<algorithm>="` (e.g. `"sha256="`) | Prefix to strip before decoding. Pass `""` to opt out. |
| `signature_encoding` | no | `"hex"` | `"hex"` or `"base64"`. |
| `algorithm` | no | `"sha256"` | `"sha256"` or legacy `"sha1"` when `allow_legacy_sha1` is true. |
| `allow_legacy_sha1` | no | `false` | Explicit opt-in for existing providers that still sign with HMAC-SHA1. |
| `delivery_id_header` | yes | — | Header name carrying the delivery id, e.g. `"x-github-delivery"`. |
| `topic` | yes | — | Event-log topic to append accepted deliveries onto. |
| `dedupe_ttl_seconds` | no | 86400 (24h) | How long delivery ids stay claimed in the inbox. |

### `webhook_intake_feed(intake_id, request) -> dict`

Feed a delivery through the substrate. `request` keys:

| Key | Required | Description |
|---|:---:|---|
| `headers` | yes | Header dict (case-insensitive lookup). |
| `body` | yes | String or bytes; HMAC is computed over this verbatim. |
| `path` | no | If the intake declared a `path`, deliveries with a different path are rejected. |
| `received_at` | no | Override the timestamp recorded on the published event. RFC3339. |

Returns:

```json
{
  "status": "accepted" | "duplicate" | "rejected",
  "intake_id": "...",
  "topic": "...",
  "delivery_id": "...",
  "topic_event_id": 123,
  "reason": "...",
  "received_at": "..."
}
```

On `accepted`, the substrate appends a `webhook_delivery` event to the
configured topic with payload:

```json
{
  "intake_id": "...",
  "delivery_id": "...",
  "received_at": "...",
  "headers": { "...": "..." },
  "body_b64": "<base64>",
  "body_text": "<utf-8 if valid, else null>",
  "path": "...",
  "signature_header": "...",
  "delivery_id_header": "...",
  "algorithm": "sha256"
}
```

The published payload is **opaque** at this layer — connectors decode it
into provider-specific shapes downstream.

### `webhook_intake_recent(intake_id, limit?) -> list`

Bounded replay buffer. Reads up to `limit` (default 32) accepted
deliveries from the intake's topic. Useful for connector-side replay or
debugging.

### `webhook_intake_list() -> list`

Snapshot of every currently-registered intake.

### `webhook_intake_deregister(intake_id) -> bool`

Remove an intake. The published deliveries on the topic remain.

## A connector wires intake in <30 lines

```harn,ignore
import "std/triggers"

let intake = webhook_intake_register({
  id: "github",
  path: "/hooks/github",
  secret: secret_get("github/webhook-secret"),
  signature_header: "x-hub-signature-256",
  delivery_id_header: "x-github-delivery",
  topic: "github.events",
})

// In your inbound HTTP handler:
let outcome = webhook_intake_feed(intake.id, {
  headers: request.headers,
  body: request.body,
  path: request.path,
})

if outcome.status == "rejected" {
  return { status: 401, body: outcome.reason }
}
return { status: 202 }
```

## Audit and observability

- Accepted deliveries land on the topic the connector chose.
- Rejections are appended to `triggers.webhook_intake.rejections` with
  the intake id, the configured topic, the path, and the reason. This is
  the audit trail when a forge sends a bad signature, a missing delivery
  id, or hits the wrong path.
- Delivery-id claims are written to the existing trigger inbox topic, so
  dedupe survives process restart for the configured TTL.

## Replay fixtures

`webhook_intake_feed` does not require a live HTTP listener. To drive
the substrate from a recorded delivery in a test or replay job, build the
request dict directly:

```harn,ignore
let outcome = webhook_intake_feed("github", {
  headers: load_json("fixtures/github_pr_opened_headers.json"),
  body: read_file("fixtures/github_pr_opened.body"),
})
```

The connector contract `[[fixtures]]` blocks (see
[connector authoring](../connectors/authoring.md)) compose the same way:
each fixture is just a `(headers, body)` pair the harness feeds in.

## Boundary

This substrate intentionally does **not** know about GitHub, GitLab,
Linear, Slack, etc. It is the shared cross-forge plumbing used by every
connector. Per-forge event mapping lives in each connector repo (e.g.
[harn-github-connector](https://github.com/burin-labs/harn-github-connector)).
