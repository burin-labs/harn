# Connector authoring

Custom connectors can now be authored in two ways:

- Rust implementations that implement `harn_vm::connectors::Connector`
- `.harn` modules loaded through `[[providers]]` manifest entries

The initial surface lives in `crates/harn-vm/src/connectors/` because the
supporting abstractions it depends on today already live in `harn-vm`:

- `EventLog` for audit and durable event plumbing
- `SecretProvider` for signing secrets and outbound tokens
- `TriggerEvent` for the normalized inbound envelope

If the connector ecosystem grows large enough, the module can be extracted into
a dedicated crate later without changing the core trait contract.

## Provider catalog

Connectors should treat the runtime `ProviderCatalog` as the authoritative
discovery surface for provider metadata. Each provider entry carries:

- the normalized payload schema name exposed through `std/triggers`
- supported trigger kinds such as `webhook` or `cron`
- outbound method names (empty today for the built-in providers)
- required secrets, including the namespace each secret must live under
- signature verification strategy metadata
- runtime connector metadata indicating whether the provider is backed by a
  built-in connector or a placeholder implementation

Harn also exposes that same catalog to scripts through
`import "std/triggers"` and `list_providers()`, so connector metadata has one
runtime-facing source instead of separate registry and docs tables.

## Harn module connectors

Root manifests can override a provider's connector implementation:

```toml
[[providers]]
id = "echo"
connector = { harn = "./echo_connector.harn" }

[[triggers]]
id = "echo-webhook"
kind = "webhook"
provider = "echo"
path = "/hooks/echo"
match = { path = "/hooks/echo", events = ["echo.received"] }
handler = "handlers::on_echo"
```

The referenced `.harn` module must export:

```harn,ignore
pub fn provider_id() -> string
pub fn kinds() -> list
pub fn payload_schema() -> dict
```

Optional lifecycle exports:

```harn,ignore
pub fn init(ctx)
pub fn activate(bindings)
pub fn shutdown()
pub fn call(method, args)
pub fn poll_tick(ctx)
```

Inbound providers must also export:

```harn,ignore
pub fn normalize_inbound(raw) -> dict
```

`normalize_inbound(raw)` returns a dict with:

- `type`: one of `"event"`, `"batch"`, `"immediate_response"`, or `"reject"`

For a single event, return:

```harn
{
  type: "event",
  event: {
    kind: "echo.received",
    occurred_at: raw.received_at,
    dedupe_key: "echo:" + body.id,
    payload: body,
  },
}
```

For multiple events, return:

```harn
{
  type: "batch",
  events: [
    {
      kind: "echo.received",
      dedupe_key: "echo:" + first.id,
      payload: first,
    },
    {
      kind: "echo.received",
      dedupe_key: "echo:" + second.id,
      payload: second,
    },
  ],
}
```

For ack-first webhooks such as URL verification handshakes, return an
immediate HTTP response and optionally include `event` or `events` to enqueue
after normalization:

```harn
{
  type: "immediate_response",
  immediate_response: {
    status: 200,
    headers: {"content-type": "text/plain; charset=utf-8"},
    body: body.challenge,
  },
}
```

For unsupported or failed verification inputs, return:

```harn
{
  type: "reject",
  status: 403,
  body: {error: "verification_failed"},
}
```

Each event dict contains:

- `kind`: normalized trigger kind
- `dedupe_key`: stable delivery key
- `payload`: provider payload dict preserved as `event.provider_payload.raw`
- `occurred_at?`: optional RFC3339 timestamp
- `tenant_id?`: optional tenant override
- `headers?`: optional normalized headers
- `batch?`: optional list payload for batched deliveries
- `signature_status?`: optional `{ state = "verified" | "unsigned" | "failed", ... }`

Harn-side connectors get three connector-only builtins during connector export
execution:

- `secret_get(secret_id)` reads from the orchestrator secret providers
- `event_log_emit(topic, kind, payload, headers?)` appends to the active event log
- `metrics_inc(name, amount?)` increments a Prometheus counter rendered as `connector_custom_<name>_total`

Minimal example:

```harn
pub fn provider_id() {
  return "echo"
}

pub fn kinds() {
  return ["webhook"]
}

pub fn payload_schema() {
  return {
    harn_schema_name: "EchoEventPayload",
    json_schema: { type: "object", additionalProperties: true },
  }
}

pub fn normalize_inbound(raw) {
  let body = raw.body_json ?? json_parse(raw.body_text)
  let token = secret_get("echo/api-token")
  metrics_inc("echo_normalize_calls")
  event_log_emit("connectors.echo.lifecycle", "normalize", {
    binding_id: raw.binding_id,
  })
  return {
    type: "event",
    event: {
      kind: "echo.received",
      occurred_at: raw.received_at,
      dedupe_key: "echo:" + body.id,
      payload: {
        body: body,
        token: token,
        binding_id: raw.binding_id,
      },
    },
  }
}

pub fn call(method, args) {
  if method == "ping" {
    return { message: args.message }
  }
  throw "method_not_found:" + method
}
```

`raw` includes normalized request metadata such as `headers`, `query`,
`body_text`, `body_json` when the body is valid JSON, `received_at`,
`binding_id`, `binding_version`, and `binding_path`.

Poll-based Harn connectors declare a manifest `kind = "poll"` trigger and
export `poll_tick(ctx)`. The orchestrator calls `poll_tick` on the configured
interval and passes:

- `binding`: the activated trigger binding, including its connector config
- `binding_id`: the trigger binding id
- `tick_at`: the scheduled tick time as RFC3339 text
- `cursor`: the last persisted cursor for the binding/state key, or `nil`
- `state`: connector-owned persisted state for the binding/state key, or `nil`
- `state_key`: the durable cursor/state key
- `tenant_id`: optional configured tenant identity
- `lease`: `{ id, tenant_id }` identity metadata for the tick owner
- `max_batch_size`: optional configured event cap

The `poll` config accepts `interval`, `interval_ms`, or `interval_secs`;
`jitter`, `jitter_ms`, or `jitter_secs`; `state_key` (also accepted as
`cursor_state_key`); `tenant_id`; `lease_id`; and `max_batch_size`.
Durations use `ms`, `s`, `m`, or `h` suffixes when supplied as strings.

`poll_tick(ctx)` returns either a list of normalized event dicts or:

```harn
{
  events: [
    {
      kind: "example.changed",
      dedupe_key: "example:42",
      payload: {id: "42"},
    },
  ],
  cursor: {after: "opaque-provider-cursor"},
  state: {last_seen_id: "42"},
}
```

Returned events use the same normalized shape as `normalize_inbound`. The
runtime applies the binding dedupe key policy, writes accepted events through
the trigger inbox envelope path, and persists `cursor`/`state` so the next
tick sees them. Shutdown requests cancel future ticks and prevent long-running
poll exports from blocking clean orchestrator shutdown.

## Rust connectors

A connector implementation owns two concerns:

- Inbound normalization: verify the provider request, preserve the raw bytes,
  and normalize into `TriggerEvent`.
- Outbound callbacks: expose provider APIs through a `ConnectorClient`.

The runtime-facing surface is:

```rust
use std::sync::Arc;

use async_trait::async_trait;
use harn_vm::connectors::{
    Connector, ConnectorClient, ConnectorCtx, ConnectorError, ProviderPayloadSchema,
    RawInbound, TriggerBinding, TriggerKind,
};
use harn_vm::{ProviderId, TriggerEvent};
use serde_json::Value as JsonValue;

struct ExampleConnector {
    provider_id: ProviderId,
    kinds: Vec<TriggerKind>,
    client: Arc<ExampleClient>,
}

struct ExampleClient;

#[async_trait]
impl ConnectorClient for ExampleClient {
    async fn call(
        &self,
        method: &str,
        args: JsonValue,
    ) -> Result<JsonValue, harn_vm::ClientError> {
        let _ = (method, args);
        Ok(JsonValue::Null)
    }
}

#[async_trait]
impl Connector for ExampleConnector {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    fn kinds(&self) -> &[TriggerKind] {
        &self.kinds
    }

    async fn init(&mut self, _ctx: ConnectorCtx) -> Result<(), ConnectorError> {
        Ok(())
    }

    async fn activate(
        &self,
        _bindings: &[TriggerBinding],
    ) -> Result<harn_vm::ActivationHandle, ConnectorError> {
        Ok(harn_vm::ActivationHandle::new(self.provider_id.clone(), 0))
    }

    async fn normalize_inbound(&self, raw: RawInbound) -> Result<TriggerEvent, ConnectorError> {
        let _payload = raw.json_body()?;
        todo!("map the provider request into TriggerEvent")
    }

    fn payload_schema(&self) -> ProviderPayloadSchema {
        ProviderPayloadSchema::named("ExamplePayload")
    }

    fn client(&self) -> Arc<dyn ConnectorClient> {
        self.client.clone()
    }
}
```

## HMAC verification helper

Webhook-style connectors should reuse
`harn_vm::connectors::verify_hmac_signed(...)` instead of open-coding HMAC
checks. The helper enforces the non-negotiable rules from issue `#167`:

- verification happens against the raw request body bytes
- signature comparisons use constant-time equality
- timestamped schemes reject outside a caller-provided window
- rejection paths write an audit event to the `audit.signature_verify` topic

The helper currently supports the three MVP HMAC header styles needed by the
planned connector tickets:

- GitHub: `X-Hub-Signature-256: sha256=<hex>`
- Notion: `X-Notion-Signature: sha256=<hex>`
- Stripe: `Stripe-Signature: t=<unix>,v1=<hex>[,v1=<hex>...]`
- Standard Webhooks: `webhook-id`, `webhook-timestamp`, and
  `webhook-signature: v1,<base64>`

## Rate limiting

Connector clients should acquire outbound permits through the shared
`RateLimiterFactory`. The current implementation is intentionally small: a
process-local token bucket keyed by `(provider_id, scope_key)`. That keeps the
first landing trait-pure while giving upcoming provider clients one place to
enforce per-installation or per-tenant quotas.

## What is deliberately not here yet

This foundation PR does not define:

- outbound stdlib client wrappers for connector-specific APIs
- third-party manifest ABI for external connector packages

Those land in follow-up tickets once the shared trait, provider catalog,
runtime registry, audit, and verification primitives are in place.
