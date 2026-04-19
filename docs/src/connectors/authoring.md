# Connector authoring

Custom connectors implement the `harn_vm::connectors::Connector` trait and
plug into a `ConnectorRegistry` at orchestrator startup. The initial surface
lives in `crates/harn-vm/src/connectors/` because the supporting abstractions
it depends on today already live in `harn-vm`:

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

## Implementing a connector

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

    fn normalize_inbound(&self, raw: RawInbound) -> Result<TriggerEvent, ConnectorError> {
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
