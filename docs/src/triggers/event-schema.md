# Trigger event schema

`TriggerEvent` is the runtime's normalized envelope for inbound and synthetic
deliveries. The envelope identifies the source and delivery; `provider_payload`
holds the provider-specific body. A failed signature can be represented for
audit even when dispatch rejects the event.

## Envelope

| Field | Meaning |
|---|---|
| `id` | Runtime event id, generated as `trigger_evt_` plus a UUIDv7 by `TriggerEvent::new`. |
| `provider` | Trigger provider id, such as `github`, `cron`, or `webhook`. |
| `kind` | Provider event kind. `qualified_kind()` joins provider and kind with a dot. |
| `received_at` | Runtime receive time as RFC3339. |
| `occurred_at` | Optional provider-reported RFC3339 time. |
| `dedupe_key` | Delivery id or other idempotency key supplied by the connector. |
| `trace_id` | Trace correlation id, generated as `trace_` plus a UUIDv7 by `TriggerEvent::new`. |
| `tenant_id` | Optional tenant namespace. |
| `headers` | Headers retained by the connector. Use `redact_headers` before retaining sensitive inbound headers. |
| `batch` | Optional array of original events attached to an aggregated delivery. |
| `raw_body` | Optional original bytes, serialized as base64. |
| `provider_payload` | Provider-tagged normalized body, described below. |
| `signature_status` | `{ state: "verified" }`, `{ state: "unsigned" }`, or `{ state: "failed", reason }`. |

`TriggerEvent::new` sets `batch` and `raw_body` to absent and the transient
`dedupe_claimed` flag to false. Callers provide the provider, kind, dedupe key,
headers, payload, and signature result. The flag is deliberately omitted from
serialization and deserializes as false; it records an in-process inbox claim,
not a durable property of the delivery. The public fields allow connectors to
attach `batch` or `raw_body` after construction.

`ProviderId` and `TenantId` wrap caller-provided strings. The event envelope
does not parse a provider's kind or dedupe key. Catalog registration validates
the provider id used for normalization, and the connector owns its delivery
identity and signature policy.

## Provider payloads

`ProviderPayload` is either a known runtime payload or a package extension.
The `provider` tag determines the known variant; the extension also carries
`schema_name`. `provider()` returns that tag in either case.

| Provider | Payload fields beyond its provider tag |
|---|---|
| `cron` | `cron_id`, `schedule`, `tick_at`, `raw` |
| `webhook` | `source`, `content_type`, `raw` |
| `a2a-push` | `task_id`, `task_state`, `artifact`, `sender`, `actor_chain`, `kind`, `raw` |
| `kafka`, `nats`, `pulsar`, `postgres-cdc`, `email`, `websocket` | Shared stream fields: `event`, `source`, `stream`, `partition`, `offset`, `key`, `timestamp`, `headers`, `raw` |
| `channel` | `id`, `name`, `name_resolved`, `scope`, `scope_id`, `payload`, `emitted_by`, and optional tenant, session, and pipeline ids |
| Package extension | `provider`, `schema_name`, `raw` |

For direct package-connector ingress, `raw` is the normalized JSON returned by
the package. The package may also retain its provider-native body inside that
JSON. Catalog normalization wraps the structured JSON supplied by its caller.

The runtime's cron normalizer reads `cron_id`, `schedule`, and `tick_at` from
`raw`; an absent or invalid tick time falls back to the current UTC time. The
generic webhook normalizer reads `X-Webhook-Source` and `Content-Type` from
the supplied header map. A2A push extracts the task state and artifact,
normalizes `cancelled` to `canceled`, and derives its payload `kind` from the
state. Stream normalizers use the same ordered field aliases for source,
stream, partition, offset, key, and timestamp while preserving the supplied
headers and `raw`. Normalization does not discard unrecognized fields in
`raw`. Channel events are built directly from `emit_channel` and do not use a
provider-catalog normalizer.

| Stream field | First present string or integer among these keys |
|---|---|
| `source` | `source`, `connector`, `origin` |
| `stream` | `stream`, `topic`, `subject`, `channel`, `mailbox`, `slot` |
| `partition` | `partition`, `shard`, `consumer` |
| `offset` | `offset`, `sequence`, `lsn`, `message_id` |
| `key` | `key`, `message_key`, `id`, `event_id` |
| `timestamp` | `timestamp`, `occurred_at`, `received_at`, `ts` |

Integer values in these fields become decimal strings; other JSON types are
skipped while searching the aliases.

The generated `GitForgePullRequestEvent` and related Git forge records are
shared connector data types, not additional `ProviderPayload` variants.

## Provider catalog

A provider registration captures one `ProviderMetadata` description. Metadata
owns the provider id, advertised kinds,
payload schema name, outbound methods, secret requirements, signature policy,
and runtime connector classification. `supports_kind` searches the advertised
kinds; it does not prevent normalization of another kind.

`ProviderCatalog::default()` starts empty. `with_defaults()` installs cron,
generic webhook, A2A push, and the six stream providers. `with_defaults_and`
adds package registrations to those defaults. Catalog reads return metadata
and schema-name projections in provider-id order.

| Catalog operation | Observable result |
|---|---|
| `register(metadata)` | Adds one package provider or reports a duplicate or invalid description. |
| `merge(metadata_list)` | Installs a batch atomically, accepting exact package reloads. |
| `metadata_for(provider)` | Returns a copy of one description, or `None`. |
| `entries()` | Returns copies of every description in provider-id order. |
| `schema_names()` | Returns provider-to-schema names in provider-id order. |
| `normalize(provider, kind, headers, raw)` | Returns the tagged payload or an unknown-provider or invalid-payload error. |

`ProviderMetadata::required_secret_names()` returns only requirements marked
`required`; `supports_kind()` checks the advertised kind list. Neither method
changes admission or normalization.

`register` strictly rejects a duplicate provider id. `merge` accepts an
identical package description again, including during reload, and rejects a
different description for an existing id. A package cannot replace a built-in
provider, even with matching metadata.
It checks the whole batch before installing any entry. Provider ids must be
nonempty and unpadded, payload schema names must be nonempty, and package
registrations cannot claim a built-in runtime connector. The process
catalog is shared by package loads; `register_provider_metadata` merges into it,
while `reset_provider_catalog` restores only the built-in registrations.

`ProviderPayload::normalize` looks up the captured registration, then releases
the catalog lock. An in-progress normalization keeps that registration even if
the catalog is reset concurrently. A built-in provider uses its runtime
normalizer. A package provider gets an extension payload whose provider and
schema tags come from the registration; its connector owns provider-native
inbound normalization.
An unknown provider fails lookup, and a built-in normalizer producing another
provider tag fails validation. Connectors that construct `TriggerEvent`
directly retain responsibility for their event fields.

## Header redaction

`HeaderRedactionPolicy` names the shared redaction policy used for trigger
headers. `redact_headers` applies it to a header map and returns a new map.
The default policy retains delivery and event metadata while redacting
sensitive names such as `Authorization`, `Cookie`, and names containing
`secret`, `token`, or `key`. Supplying headers to `TriggerEvent::new` does not
redact them automatically.
