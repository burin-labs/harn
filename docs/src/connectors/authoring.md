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

## External connector repository guidance

This page is the canonical authoring guide for first-party connector package
repositories. Repo-local `CLAUDE.md` and `AGENTS.md` files should stay as thin
pointers plus provider-specific notes.

Keep repo-local guidance limited to details that differ by provider:

- webhook header names, signature schemes, and replay windows
- auth token shapes, API base URLs, and host-specific endpoint caveats
- polling caveats, if the provider has a poll-based surface
- dependency boundaries such as a sibling SDK package that owns outbound API
  definitions

Do not copy shared Harn syntax, package layout, connector export contracts,
fixture schema, effect-policy rules, or test command matrices into connector
repos. If a shared instruction is missing, add it here first and point external
repos at this page.

Each connector repo should run a cheap guidance guard in CI before expensive
Harn setup:

```yaml
- name: Check connector guidance is canonical
  shell: bash
  run: |
    set -euo pipefail
    guidance_files=()
    for file in CLAUDE.md AGENTS.md; do
      if [[ -f "${file}" ]]; then
        guidance_files+=("${file}")
      fi
    done
    if [[ "${#guidance_files[@]}" -eq 0 ]]; then
      echo "Add CLAUDE.md or AGENTS.md with a pointer to the canonical connector authoring guide." >&2
      exit 1
    fi
    copied_guidance='(^## (Quick repo conventions|How to test|Reference Rust impl|Upstream conventions|Harn module connectors|Connector package gate|Rust connectors|Testing|Development)$|File extension:|Entry point:|Tests live under|Run targeted checks|Run checks|cargo install harn-cli|harn --version|harn install|harn (check|lint|fmt|connector (check|test))|for test in tests/\*\.harn)'
    for file in "${guidance_files[@]}"; do
      if ! grep -Eq 'docs/src/connectors/authoring\.md|docs\.harnlang\.com/connectors/authoring\.html' "${file}"; then
        echo "${file} must link to docs/src/connectors/authoring.md instead of restating it." >&2
        exit 1
      fi
      if grep -Eiq "${copied_guidance}" "${file}"; then
        echo "${file} is re-implementing canonical Harn authoring guidance; keep only provider-specific notes." >&2
        exit 1
      fi
    done
    if [[ -f CLAUDE.md ]] && ! grep -Eq '^## Provider Notes$' CLAUDE.md; then
      echo "CLAUDE.md must keep local content under a Provider Notes section." >&2
      exit 1
    fi
```

## Harn module connectors

Root manifests can override a provider's connector implementation:

```toml
[[providers]]
id = "echo"
connector = { harn = "./echo_connector.harn" }
capabilities = ["webhook", "oauth", "rate_limit", "pagination"]
oauth = {
  resource = "https://api.echo.example/",
  authorization_endpoint = "https://auth.echo.example/oauth/authorize",
  token_endpoint = "https://auth.echo.example/oauth/token",
  scopes = "echo.read echo.write",
}

[[triggers]]
id = "echo-webhook"
kind = "webhook"
provider = "echo"
path = "/hooks/echo"
match = { path = "/hooks/echo", events = ["echo.received"] }
handler = "handlers::on_echo"
```

The optional `oauth` table is package-owned setup metadata consumed by
`harn connect <provider>`. It supports `resource`,
`authorization_endpoint`, `token_endpoint`, `registration_endpoint`, `scopes`,
`client_id`, `client_secret`, and `token_endpoint_auth_method`; operator CLI
flags override those values for a single run.

The optional `capabilities` declaration feeds `harn check --connector-matrix`
and the generated connector parity docs. Declare any of `webhook`, `oauth`,
`rate_limit`, `pagination`, `graphql`, and `streaming` that the package
supports. Hyphenated names such as `rate-limit` are accepted in manifests and
CLI filters.

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

Connector exports run under a default effect policy. `normalize_inbound(raw)` is
the ingress hot path, so its default policy allows deterministic local work plus
`secret_get`, `event_log_emit`, and `metrics_inc`, while rejecting outbound
network calls, LLM calls, process execution, connector client calls, host calls,
MCP calls, and ambient filesystem/project access. This keeps webhook ack paths
fast and testable without external dependencies.

`poll_tick(ctx)` and `call(method, args)` use the connector-outbound class:
they may use `connector_call` and normal network builtins, but still reject
ambient filesystem/project access, process execution, LLM calls, host calls, and
MCP calls unless a trusted host overrides the policy. `activate(bindings)` uses
the activation class, which permits connector/network setup work under the same
filesystem/process/LLM restrictions.

Hosts embedding `HarnConnector` can override defaults for trusted private
connectors with `HarnConnector::load_with_effect_policies` and
`HarnConnectorEffectPolicies`. For example, call `trust_export("poll_tick")` to
run that export without the default connector policy, or `set_export_policy` to
install a narrower host-specific `CapabilityPolicy`.

## Connector package gate

Pure-Harn connector packages should run the package-level gate in CI:

```bash
harn connector test .
```

The gate validates package metadata, runs `harn check`, `harn lint`,
`harn fmt --check`, executes package-local `tests/*.harn` fixture programs,
checks install/import behavior from a clean consumer package, parses standalone
Harn doc examples, and includes the connector contract check below. Pass
`--json` to emit a machine-readable readiness report for CI, Harn Cloud, or
Burin Code.

Use the lower-level contract harness when iterating only on the connector
module:

```bash
harn connector check .
```

That command loads the package through its `harn.toml` `[[providers]]` entries,
uses the normal Harn-backed connector adapter, and checks connector contract
v1:

| Export | Required | Checked behavior |
|---|---:|---|
| `provider_id()` | Yes | Returns a non-empty string matching the manifest provider id |
| `kinds()` | Yes | Returns at least one non-empty trigger kind string |
| `payload_schema()` | Yes | Returns `{harn_schema_name, json_schema?}` compatible with `ProviderPayloadSchema` |
| `normalize_inbound(raw)` | For inbound fixtures | Returns a supported `NormalizeResult` v1 shape |
| `init(ctx)` | No | Runs with in-memory event log, secrets, metrics, inbox, and rate-limit handles |
| `activate(bindings)` | No | Accepts deterministic bindings for non-poll kinds |
| `shutdown()` | No | Runs after checks so connector cleanup paths are exercised |
| `call(method, args)` | No | May return data or throw `method_not_found:<method>` for an unknown probe method |
| `poll_tick(ctx)` | Required for `poll` kind | Presence is checked by default; pass `--run-poll-tick` to execute the first tick |

The harness catches common drift such as returning a raw schema object with a
`name` field instead of `harn_schema_name`, or returning an ack wrapper like
`{ immediate_response, event }` without the required `type =
"immediate_response"` discriminator. It also runs connector-effect-policy
diagnostics before fixtures, so direct hot-path calls such as `http_get`,
`llm_call`, or `read_file` inside `normalize_inbound` fail with an author-facing
message.

Packages can declare deterministic normalize fixtures in `harn.toml`:

```toml
[connector_contract]
version = 1

[[connector_contract.fixtures]]
provider = "slack"
name = "url verification"
kind = "webhook"
headers = { "content-type" = "application/json" }
body_json = { type = "url_verification", challenge = "challenge-token" }
expect_type = "immediate_response"
expect_response_status = 200
expect_response_body = "challenge-token"
expect_event_count = 0
```

Fixture fields:

| Field | Description |
|---|---|
| `provider` | Manifest provider id to exercise |
| `name` | Optional display name for failures and JSON output |
| `kind` | Raw inbound kind passed to the connector, defaulting to `webhook` |
| `headers` | Request headers as a TOML table |
| `query` | Optional query parameters as a TOML table |
| `metadata` | Optional raw inbound metadata; defaults include binding id/version/path |
| `body` | Raw request body text |
| `body_json` | JSON request body encoded as TOML |
| `expect_type` | Optional expected NormalizeResult type: `event`, `batch`, `immediate_response`, or `reject` |
| `expect_kind` | Optional expected normalized event kind |
| `expect_dedupe_key` | Optional exact normalized event dedupe key |
| `expect_signature_state` | Optional normalized signature state: `verified`, `unsigned`, or `failed` |
| `expect_payload_contains` | Optional TOML/JSON subset that must be present in the serialized `provider_payload`; use this for Rust-shape parity fixtures |
| `expect_response_status` | Optional HTTP status expected for `immediate_response` or `reject` results |
| `expect_response_body` | Optional exact body expected for `immediate_response` or `reject` results |
| `expect_event_count` | Optional expected number of normalized events |
| `expect_error_contains` | Optional substring expected in a deterministic `normalize_inbound` error, useful for proving denied effects fail without touching real services |

Use `--provider <id>` to check one provider from a multi-provider package and
`--json` for machine-readable CI output.

Connector packages must also declare setup metadata on each `[[providers]]`
entry so GUI, TUI, and CLI hosts can render the same Connect/Fix experience
without provider-specific code:

```toml
[[providers]]
id = "example"
connector = { harn = "./lib.harn" }
capabilities = ["webhook", "oauth"]

[providers.setup]
auth_type = "oauth2"
flow = "browser"
required_scopes = ["example.read", "example.write"]
required_secrets = []
setup_command = ["harn", "connect", "example"]
validation_command = ["harn", "connect", "status", "--connector", "example", "--json"]

[[providers.setup.health_checks]]
id = "credentials"
kind = "command"
command = ["harn", "connect", "status", "--connector", "example", "--json"]

[providers.setup.recovery]
missing_auth = "Run `harn connect example`."
expired_credentials = "Refresh or reconnect the OAuth token."
revoked_credentials = "Revoke the stale local token, then reconnect."
missing_scopes = "Reconnect with the scopes listed in required_scopes."
inaccessible_resource = "Grant the connector access to the requested resource."
transient_provider_outage = "Retry after the provider or credential backend recovers."
```

`auth_type` names the credential family (`oauth2`, `device-code`, `api-key`,
`github-app`, or `none`) and `flow` names the host interaction. `health_checks`
can be `secret`, `command`, `http`, `mcp`, or `resource`; only `secret` and
`command` are evaluated by `harn connect status`, while the remaining kinds are
declared for hosts and provider-specific validators. `harn connector check`
fails when setup metadata is missing or malformed.

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
checks. The helper enforces these non-negotiable rules:

- verification happens against the raw request body bytes
- signature comparisons use constant-time equality
- timestamped schemes reject outside a caller-provided window
- rejection paths write an audit event to the `audit.signature_verify` topic

The helper supports the raw-body HMAC header styles used by first-party
connector packages and generic webhook intake:

- GitHub: `X-Hub-Signature-256: sha256=<hex>`
- Notion: `X-Notion-Signature: sha256=<hex>`
- Stripe: `Stripe-Signature: t=<unix>,v1=<hex>[,v1=<hex>...]`
- Standard Webhooks: `webhook-id`, `webhook-timestamp`, and
  `webhook-signature: v1,<base64>`

Harn-authored connector packages can import `std/connectors/shared` for the
package-level equivalent helpers:

```harn
import {
  connector_http_json,
  connector_http_rate_limit,
  connector_http_request,
  oauth2_token_refresh,
  paginate_cursor,
  rate_limit_token_bucket,
  verify_hmac_signature,
  verify_jwt,
} from "std/connectors/shared"
```

Use the Rust `verify_hmac_signed(...)` path for runtime HTTP ingress when you
need timestamp-window checks, audit events, or a provider-specific signed
message format. Use `std/connectors/shared` inside Harn package exports for
local HMAC checks, JWT/JWKS verification, outbound HTTP policy, OAuth2 token
refresh, package-local token buckets, and cursor pagination.

## Outbound HTTP policy

Connector packages should layer provider-specific request logic over
`connector_http_request(...)` or `connector_http_json(...)` instead of
open-coding retry loops around `harness.net.request(...)`. The raw
`harness.net.*` APIs remain the escape hatch when a package needs exact client
behavior; the shared policy wrapper is the default for provider API calls that
need stable error categories, idempotency-aware retries, and rate-limit
metadata.

```harn
import { connector_http_json } from "std/connectors/shared"

fn api_json(method, url, token, body = nil, idempotency_key = nil) {
  return connector_http_json(
    method,
    url,
    {
      provider: "example",
      operation: "api_json",
      headers: {Authorization: "Bearer " + token, Accept: "application/json"},
      body: if body == nil { nil } else { json_stringify(body) },
      idempotency_key: idempotency_key,
      retry: {max_attempts: 3, base_ms: 250, cap_ms: 30000},
    },
  )
}
```

`connector_http_request(...)` returns a non-throwing envelope. Successful
responses contain `{ok: true, status, headers, body, retry_after_ms?}`.
Failures contain `{ok: false, status?, retryable, retry_after_ms?, error}` where
`error.category` is stable enough for generated SDKs and package code to branch
on (`"rate_limit"`, `"overloaded"`, `"server_error"`, `"auth"`,
`"permission"`, `"not_found"`, `"timeout"`, `"invalid_json"`, and transport
categories such as `"transient_network"` or `"egress_blocked"`).

The retry policy uses total-attempt semantics:
`retry: {max_attempts, base_ms, cap_ms}`. Safe/idempotent methods (`GET`,
`HEAD`, `PUT`, `DELETE`, `OPTIONS`) may retry retryable statuses. `POST` and
`PATCH` retry only when an `Idempotency-Key` header is already present, when
`options.idempotency_key` can add one, or when the caller explicitly sets
`retry_unsafe: true`. When a provider returns a `Retry-After` value above
`cap_ms`, the wrapper returns a retryable error with `retry_after_ms` instead of
sleeping for a long reset window.

Use `connector_http_header(...)` for case-insensitive response header lookup and
`connector_http_rate_limit(...)` to expose `Retry-After`, `RateLimit-*`, and
`X-RateLimit-*` metadata without repeating provider-local header scans.

## Rate limiting

Connector clients should acquire outbound permits through the shared
`RateLimiterFactory`. The current implementation is intentionally small: a
process-local token bucket keyed by `(provider_id, scope_key)`. That keeps the
first landing trait-pure while giving upcoming provider clients one place to
enforce per-installation or per-tenant quotas.

## Ownership boundary

New provider business logic belongs in Harn connector packages, not in new
Rust-side provider modules. Keep Harn core changes focused on the shared
runtime substrate: `RawInbound`, `TriggerEvent`, signing helpers, the package
contract adapter, the connector testkit, effect policy, scheduling, and
dispatcher integration. Provider packages can then ship event-specific
normalization and outbound methods on their own release cadence.
