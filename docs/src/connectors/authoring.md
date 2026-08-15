# Connector authoring

Provider connectors are `.harn` packages loaded through `[[providers]]`
manifest entries. Rust owns only the provider-neutral runtime substrate in
`crates/harn-vm/src/connectors/`:

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
- outbound method names declared by the owning package
- required secrets, including the namespace each secret must live under
- signature verification strategy metadata
- runtime connector metadata indicating whether a core transport is built in
  or a package supplies the provider implementation

Harn also exposes that same catalog to scripts through
`import "std/triggers"` and `list_providers()`, so connector metadata has one
runtime-facing source instead of separate registry and docs tables.

## External connector repository guidance

This page is the canonical authoring guide for first-party connector package
repositories. Each repo keeps one `AGENTS.md` holding a pointer here plus its
provider-specific notes, and a `CLAUDE.md` symlinked to it so the two names
cannot drift apart.

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
    if [[ -f AGENTS.md ]] && ! grep -Eiq '^## Provider notes$' AGENTS.md; then
      echo "AGENTS.md must keep local content under a Provider notes section." >&2
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
pub fn init(harness: Harness, ctx)
pub fn activate(harness: Harness, bindings)
pub fn shutdown(harness: Harness)
pub fn call(harness: Harness, method, args)
pub fn poll_tick(harness: Harness, ctx)
```

After `harn install` materializes a dependency, both `harn run` and
`harn run --serve mcp` activate its declared connector implementations for the
lifetime of that process. Calls through
`harness.net.connector_call(provider, method, args)` therefore reach the
package's `call` export without host-specific registration. A root manifest
entry for the same provider takes precedence over a dependency declaration,
and a Harn implementation takes precedence over the corresponding Rust
builtin.

Inbound providers must also export:

```harn,ignore
pub fn normalize_inbound(harness: Harness, raw) -> dict
```

Static metadata exports are pure. Every runtime export is an execution
boundary and must declare `harness: Harness` as its first parameter; contract
loading rejects an untyped or ambient entrypoint before activation. Inside an
export, pass only the narrow nominal handle a helper needs—for example,
`verify_signature(harness.secrets, raw)` or
`record_delivery(harness.obs, event)`.

`normalize_inbound(harness, raw)` returns a dict with:

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

Connector-local effects use the same Harness capabilities as every other Harn
program:

- `harness.secrets.read(secret_id)` reads from the orchestrator secret providers
- `harness.obs.event_log_emit(topic, kind, payload, headers?)` appends to the active event log
- `harness.obs.metrics_inc(name, amount?)` increments a Prometheus counter rendered as `connector_custom_<name>_total`

Connector exports run under a default effect policy.
`normalize_inbound(harness, raw)` is the ingress hot path, so its default policy
allows deterministic local work plus secret reads, observability emission,
clock reads, and connector-state reads, while rejecting outbound
network calls, LLM calls, process execution, connector client calls, host calls,
MCP calls, and filesystem/project access. This keeps webhook ack paths
fast and testable without external dependencies.

`poll_tick(harness, ctx)` and `call(harness, method, args)` use the
connector-outbound class: they may use `harness.net.connector_call` and normal
`harness.net` methods, but still reject filesystem/project access, process
execution, LLM calls, host calls, and MCP calls unless a trusted host overrides
the policy. `activate(harness, bindings)` uses the activation class, which
permits connector/network setup work under the same filesystem/process/LLM
restrictions.

Hosts embedding `HarnConnector` can override defaults for trusted private
connectors with `HarnConnector::load_with_effect_policies` and
`HarnConnectorEffectPolicies`. For example, call `trust_export("poll_tick")` to
run that export without the default connector policy, or `set_export_policy` to
install a narrower host-specific `CapabilityPolicy`.

## Connector package gate

Pure-Harn connector packages should run the package-level gate in CI:

```bash
harn package verify .
```

The gate validates package metadata, runs `harn check`, `harn lint`,
`harn fmt --check`, executes package-local `tests/*.harn` fixture programs,
checks install/import behavior from a clean consumer package, parses standalone
Harn doc examples, and includes the connector contract check below. Pass
`--json` to emit a machine-readable readiness report for CI, cloud platforms, or
IDE hosts.

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
| `normalize_inbound(harness, raw)` | For inbound fixtures | Returns a supported `NormalizeResult` v1 shape |
| `init(harness, ctx)` | No | Runs with an isolated Harness backed by in-memory connector services |
| `activate(harness, bindings)` | No | Accepts deterministic bindings for non-poll kinds |
| `shutdown(harness)` | No | Runs after checks so connector cleanup paths are exercised |
| `call(harness, method, args)` | No | May return data or throw `method_not_found:<method>` for an unknown probe method |
| `poll_tick(harness, ctx)` | Required for `poll` kind | Presence is checked by default; pass `--run-poll-tick` to execute the first tick |

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
| `expect_payload_contains` | Optional TOML/JSON subset that must be present in the serialized package-owned `provider_payload` |
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

Connector contract v2 requires each provider to declare one product-facing
service contract. This is the semantic source for host setup screens,
capability filtering, action
policy, protected-profile disclosure, spend presentation, reconciliation, and
redaction. Provider request and response schemas do not belong here; keep them
inside the connector adapter.

```toml
[connector_contract]
version = 2

[providers.service]
name = "Example Travel"
description = "Searches current travel offers and creates governed orders."

[[providers.service.operations]]
id = "offers.search"
capability = "travel.search"
purpose = "Find current offers for the requested itinerary."
effect = "read"
environments = ["test", "live"]
evidence = ["citation", "current_provider_state"]
redaction = ["error_body"]

[[providers.service.operations.parameters]]
name = "origin"
description = "IATA code the itinerary departs from."
type = "string"
required = true

[[providers.service.operations.parameters]]
name = "limit"
description = "How many offers to return."
type = "integer"

[[providers.service.operations.parameters]]
name = "cabin"
description = "Cabin class to search."
type = "string"
allowed_values = ["economy", "premium_economy", "business", "first"]

[[providers.service.operations]]
id = "orders.create"
capability = "travel.booking"
purpose = "Create the exact order reviewed by the user."
effect = "consequential"
environments = ["test"]
evidence = ["fresh_quote", "user_confirmation"]
external_spend = "commit"
reconciliation = "required"
test_profile = "fictional_required"
redaction = ["request_body", "response_body", "error_body"]

[providers.service.operations.protected_profile]
required = ["legal_identity", "birth_date"]
optional = ["contact_details", "loyalty_accounts", "accessibility_needs"]

[[providers.service.operations.protected_profile.conditional]]
condition = "international_itinerary"
field_classes = ["travel_documents"]
```

The closed protected-profile classes are `legal_identity`, `birth_date`,
`contact_details`, `accessibility_needs`, `loyalty_accounts`, and
`travel_documents`. They describe disclosure classes only; profile values must
never appear in the manifest, action intent, transcript, receipt, or logs.
Operations that use profile data in `test` mode must set
`test_profile = "fictional_required"`. Hosts then supply an explicitly
fictional fixture instead of silently treating test data as the user.

Each operation may declare the arguments it accepts under `parameters`. This is
what lets a host project the operation into an agent tool without a
hand-written, per-connector argument table; without it the model is guessing
argument names. Each entry needs a `name` (ASCII letters, digits, `.`, `_`, or
`-`, unique within the operation) and a `description`, which is the text the
model reads. `type` is one of `string`, `integer`, `number`, `boolean`,
`object`, or `array`. `required` defaults to `false`. `allowed_values` declares
a closed set and applies only to a `string` parameter.

The closed vocabulary is deliberate: it stays authorable in TOML and validated
at the manifest boundary, and hosts widen it into whatever schema dialect their
tool surface speaks. It describes argument *shape* only — provider request and
response schemas still belong inside the connector adapter.

`parameters` is optional and omitting it stays valid. A connector repository
pins a Harn version, and this manifest rejects unknown keys, so a connector
cannot declare parameters until a release carrying the field reaches it. Hosts
must keep projecting an operation that declares none, falling back to
free-form arguments, rather than reading an empty list as "takes no arguments".

`harn connect status --json` and `harn connect setup-plan --json` project the
same typed service block alongside authentication and health state. Hosts
should consume that projection instead of maintaining connector-specific
tables.

`auth_type` names the credential family (`oauth2`, `device-code`, `api-key`,
`github-app`, or `none`) and `flow` names the host interaction. `health_checks`
can be `secret`, `command`, `http`, `mcp`, or `resource`; only `secret` and
`command` are evaluated by `harn connect status`, while the remaining kinds are
declared for hosts and provider-specific validators. `harn connector check`
fails when setup metadata is missing or malformed.

For an API key that may come from a process environment variable, map the
logical secret to its accepted names. Harn checks only whether a declared
variable has a non-blank value. Status output includes its name, never its
value.

```toml
credential_environment = [
  { secret = "example/api-key", environment_names = ["EXAMPLE_API_KEY"] },
]
```

Every mapped `secret` must also appear in `required_secrets`. Names must use
the portable `A_Z`, `0_9`, and `_` environment-variable form. This manifest
mapping is the connector's source of truth; hosts must not guess names from a
provider id.

### Declared secrets arrive in `args.secrets`

When an operation dispatches, the runtime resolves the provider's
`required_secrets` from the secret store and passes them in `args.secrets`,
keyed by the secret name with `-` replaced by `_`. A manifest declaring
`required_secrets = ["example/api-key"]` reaches the connector as
`args.secrets.api_key`, so `call` should read that first:

```harn
const api_key = args?.api_key ?? args?.secrets?.api_key ?? env.get("EXAMPLE_API_KEY")
```

A credential the caller passed explicitly wins over the stored one, and a
declared secret the store cannot produce is simply absent rather than an error
— manifests list inbound webhook secrets beside outbound API credentials, and
no single operation needs all of them. Report the specific missing credential
from the operation that needed it.

When an `api-key` provider declares exactly one required secret,
`harn connect <provider>` prompts for that value and stores it at the declared
secret id. It also accepts `--from-env NAME` and `--value-file PATH`. Providers
with several independent secrets should keep explicit `harn connect api-key`
commands in `setup_command`; the shorthand reports those targets instead of
guessing which value belongs where.

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

pub fn normalize_inbound(harness: Harness, raw) {
  const body = raw.body_json ?? json_parse(raw.body_text)
  const token = harness.secrets.read("echo/api-token")
  harness.obs.metrics_inc("echo_normalize_calls")
  harness.obs.event_log_emit("connectors.echo.lifecycle", "normalize", {
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

pub fn call(_harness: Harness, method, args) {
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
export `poll_tick(harness, ctx)`. The orchestrator calls `poll_tick` on the
configured interval and passes:

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

`poll_tick(harness, ctx)` returns either a list of normalized event dicts or:

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

## HMAC verification helper

Webhook-style packages should reuse `verify_hmac_signature(...)` from
`std/connectors/shared` instead of open-coding HMAC checks. The helper enforces
these non-negotiable rules:

- verification happens against the raw request body bytes
- signature comparisons use constant-time equality
- legacy HMAC-SHA1 is rejected unless the package opts in explicitly

The package still owns provider-specific signed-message construction,
timestamp-window and nonce/delivery-id replay checks, and rejection audit
events. Exercise valid, expired/replayed, malformed, and wrong-secret cases in
contract fixtures.

The helper accepts a hexadecimal digest with an optional `sha256=` or `sha1=`
prefix. Packages assemble any provider-specific timestamped signed message
before calling it. The core `webhook` transport separately owns its Standard
Webhooks signature adapter.

Harn-authored connector packages import HTTP policy from
[`std/connectors/http`](../modules.md#stdconnectorshttp) and the remaining
package helpers from `std/connectors/shared`:

```harn
import {
  connector_http_json,
  connector_http_rate_limit,
  connector_http_request,
} from "std/connectors/http"
import {
  git_forge_pull_request_event,
  git_forge_pull_request_topic,
  git_forge_writeback_request,
  oauth2_token_refresh,
  paginate_cursor,
  rate_limit_token_bucket,
  verify_hmac_signature,
  verify_jwt,
} from "std/connectors/shared"
```

Use `std/connectors/http` for outbound requests. Use
`std/connectors/shared` for HMAC checks, JWT/JWKS verification, OAuth2 token
refresh, package-local token buckets, cursor pagination, and forge events.
The four `connector_http_*` names remain available from `std/connectors/shared`
for compatibility with existing packages; new code should import them from
`std/connectors/http` so HTTP policy has one visible owner.
Existing providers that still sign with HMAC-SHA1 must call
`verify_hmac_signature(..., "sha1", {allow_legacy_sha1: true})`; new
connectors should use SHA-256 or a provider-specific verifier.

## Git forge PR/MR lifecycle events

Forge connector packages should emit provider-native trigger events and, for
pull-request or merge-request lifecycle events, also emit the shared
`GitForgePullRequestEvent` shape to `git_forge_pull_request_topic()`. The raw
provider payload stays in `raw_payload`; consumers can subscribe to the shared
topic without vendoring GitHub, GitLab, or Gitea webhook adapters.

```harn
import {
  git_forge_pull_request_event,
  git_forge_pull_request_topic,
} from "std/connectors/shared"

pub fn normalize_inbound(harness: Harness, raw) {
  const body = raw.body_json ?? json_parse(raw.body_text)
  const forge = git_forge_pull_request_event("github", body)
  if forge != nil {
    harness.obs.event_log_emit(
      git_forge_pull_request_topic(),
      forge.kind,
      forge,
      {provider: "github"},
    )
  }
  const kind = if body.action == nil {
    "pull_request"
  } else {
    "pull_request." + body.action
  }
  return {
    type: "event",
    event: {
      kind: kind,
      dedupe_key: raw.headers["X-GitHub-Delivery"],
      payload: body,
      signature_status: {state: "verified"},
    },
  }
}
```

`git_forge_pull_request_event(provider, payload)` accepts GitHub
`pull_request`, GitLab `merge_request`, and Gitea/Forgejo `pull_request`
payloads. It normalizes lifecycle values to `opened`, `reopened`,
`synchronized`, `updated`, `ready_for_review`, `closed`, or `merged`, and
returns a writeback target that `git_forge_writeback_request(event, body)` can
turn into a connector call. GitHub maps to `issues.create_comment`; other forge
packages should implement the shared `git_forge.comment` outbound method.

## Outbound HTTP policy

Connector packages should layer provider-specific request logic over
`connector_http_request(...)` or `connector_http_json(...)` instead of
open-coding retry loops around `harness.net.request(...)`. The raw
`harness.net.*` APIs remain the escape hatch when a package needs exact client
behavior; the shared policy wrapper is the default for provider API calls that
need stable error categories, idempotency-aware retries, and rate-limit
metadata.

```harn
import { connector_http_json } from "std/connectors/http"

fn api_json(clock: HarnessClock, net: HarnessNet, request) {
  return connector_http_json(
    clock,
    net,
    request.method,
    request.url,
    {
      provider: "example",
      operation: "api_json",
      headers: {Authorization: "Bearer " + request.token, Accept: "application/json"},
      body: if request.body == nil { nil } else { json_stringify(request.body) },
      idempotency_key: request.idempotency_key,
      retry: {max_attempts: 3, base_ms: 250, cap_ms: 30000},
    },
  )
}
```

Pass `harness.clock` and `harness.net` from an entrypoint; reusable helpers
should accept those narrow handles instead of the root `Harness`. Grouping the
request fields in a record keeps call sites self-describing as the interface
evolves.

`connector_http_request(...)` returns a non-throwing envelope. Successful
responses contain `{ok: true, status, headers, body, retry_after_ms?}`.
Failures contain `{ok: false, status?, retryable, retry_after_ms?, error}` where
`error.category` is stable enough for generated SDKs and package code to branch
on (`"rate_limit"`, `"overloaded"`, `"server_error"`, `"auth"`,
`"permission"`, `"not_found"`, `"timeout"`, `"invalid_json"`, and transport
categories such as `"transient_network"` or `"egress_blocked"`).

Failure envelopes bound accidental disclosure. Harn replaces secret-bearing
response headers such as `Set-Cookie` and `X-Api-Key` with `[redacted]` and
limits response bodies to 1,024 characters. Set `redact_error_body: true` when
a provider error body may contain secrets or personal data. Successful
responses are unchanged. Header handling uses Harn's
[shared redaction policy](../redaction.md), including host overrides.

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

AWS-backed connector packages that need exactly one signed REST/JSON call can
use the global `aws_sigv4_headers(...)` primitive and still route the request
through `harness.net.request(...)`. Keep this as a narrow auth helper, not the
start of an AWS SDK: credentials come from the caller or secret provider, and
service clients, paginators, waiters, Smithy generation, and live AWS tests stay
out of scope for connector packages.

```harn
const body = "{\"TableName\":\"Items\"}"
const url = "https://dynamodb.us-east-1.amazonaws.com/"
http_mock("POST", url, {status: 200, body: "{\"ok\":true}", headers: {}})

const signed = aws_sigv4_headers({
  method: "POST",
  url: url,
  service: "dynamodb",
  region: "us-east-1",
  body: body,
  access_key_id: access_key_id,
  secret_access_key: secret_access_key,
  session_token: session_token,
  headers: {
    "Content-Type": "application/x-amz-json-1.0",
    "X-Amz-Target": "DynamoDB_20120810.DescribeTable",
  },
  timestamp: "20260429T120000Z",
})

const response = harness.net.request("POST", url, {
  body: body,
  headers: signed.headers,
})
```

`timestamp` is required so signing is deterministic in tests. The helper returns
`headers`, `authorization`, `amz_date`, `content_sha256`, and `signed_headers`;
it never returns derived signing keys or the AWS signer's canonicalization
internals. Validation errors identify the invalid field without echoing access
keys, secret access keys, session tokens, or signed headers. The normal
redaction policy scrubs `Authorization` and
`X-Amz-Security-Token` from recorded HTTP mock calls and transcripts unless a
test explicitly opts into sensitive values.

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
