# Outbound workflow server

`harn-serve` is the shared outbound-server crate for exposing Harn workflows to
external callers. It contains the local Agents API, MCP, A2A, and ACP adapters
plus the shared dispatch, auth, replay, and export-catalog pieces those
adapters use.

For user-facing protocol examples, start with
[MCP, ACP, and A2A integration](./mcp-and-acp.md). For a quick status table
across entry points, see [Protocol support matrix](./protocol-support.md).

The goal is to keep protocol adapters thin:

- load one `.harn` module and discover its exported `pub fn` entrypoints once
- lower Harn param and return types into adapter-facing schemas once
- authenticate inbound requests through one normalized auth policy surface
- invoke the same Harn function regardless of transport
- emit the same tracing and trust-graph records regardless of transport

## Shared-core responsibilities

The `harn-serve` crate owns these pieces:

- export catalog loading for `pub fn` entrypoints
- shared dispatch request/response types
- replay-cache hooks for idempotent replays
- cooperative cancellation wiring into the VM
- normalized auth policy types:
  API keys
  HMAC canonical-request signatures
  OAuth 2.1 claims already validated by the hosting transport
- unified observability:
  one inbound span per call
  one trust-graph record per terminal outcome
- common adapter descriptors and transport-specific modules so MCP, A2A, and
  ACP can layer their own wire protocol without duplicating shared concerns

This keeps adapter tickets focused on protocol mechanics such as discovery
documents, streaming, progress notifications, or session semantics.

## HTTP response codec

For HTTP-shaped adapters (the local Agents API and any future REST
surface), `.harn` handlers shape their replies with the `http_*`
builtins. The handler returns the result of one of these — the
adapter renders it into a real `axum::Response`:

| Builtin                                          | Result                                                  |
| ------------------------------------------------ | ------------------------------------------------------- |
| `http_ok(body)`                                  | `200 OK` + JSON `body`                                  |
| `http_created(body, location?)`                  | `201 Created` + optional `Location` header              |
| `http_no_content()`                              | `204 No Content`, body suppressed                       |
| `http_error(status, code, message, details?)`    | error envelope: `{ code, message, request_id, details? }` |
| `http_stream(source, content_type?)`             | streamed body, `source` is a list or channel of chunks  |
| `http_sse(source, retry_ms?)`                    | Server-Sent Events, `source` is a list or channel       |
| `http_reply(status, body?, headers?)`            | low-level escape hatch for arbitrary 1xx-5xx codes; a `bytes` body is rendered byte-exact |
| `http_reply_from(result)`                        | convert a `{status, body, headers, body_kind?}` record into the same tagged response envelope |

Plain values returned from a handler still work — they degrade to
`200 OK` with `Content-Type: application/json`. Untagged dicts that
happen to have a `status` key are not picked up; only the tagged
record from the builtins above is.

Adapter-level dispatch failures (auth, scope, validation, execution)
render through the same envelope, so callers see one consistent
error shape regardless of who raised the error. The `request_id`
field is filled in by the codec from the inbound request and is
always present.

## Picking an adapter

Choose the adapter based on the caller's mental model, not by protocol
popularity alone.

### Local agents API

Choose the local Agents API when a client wants an OpenAPI-described HTTP
surface instead of a protocol-specific JSON-RPC transport.

Run it with:

```bash
harn serve api agent.harn
harn serve api --bind 127.0.0.1:8787 agent.harn
harn serve api --api-key "$HARN_KEY" agent.harn
```

Behavior today:

- serves `GET /openapi.json`, `GET /health`, `GET /version`, and `/v1/*`
- creates ACP-backed sessions and runs prompts as Tasks
- streams session, task, tool, and permission updates over SSE
- forwards ACP `session/request_permission` and Harn HITL responses through
  the existing runtime path so decisions remain transcript and replay visible
- exposes local runtime, capability, provider-catalog, tool-registry,
  workspace, and UTF-8 file helpers for generated SDKs
- supports the same API-key, HMAC, and TLS listener settings as the other HTTP
  adapters

### MCP

Choose MCP when the caller wants a tool surface.

Typical fit:

- IDEs that can mount tools
- agent frameworks that already speak MCP
- clients that expect `tools/list` and `tools/call`

Mapping:

- each exported `pub fn` becomes a tool
- Harn type annotations become tool input/output schemas
- replay keys map naturally to idempotent tool invocations

Run it with:

```bash
harn serve mcp server.harn
harn serve mcp --transport http server.harn
harn serve mcp --transport http --tls edge --bind 127.0.0.1:8765 server.harn
harn serve mcp --transport http --tls self-signed-dev --bind 127.0.0.1:8765 server.harn
harn serve mcp --transport http --cert certs/prod.pem --key certs/prod-key.pem server.harn
```

Behavior today:

- stdio transport for local subprocess-style MCP clients
- Streamable HTTP `POST` / `GET` endpoint at `--path`
- legacy SSE compatibility endpoints at `--sse-path` and `--messages-path`
- TLS listener modes:
  `plain` for intentional HTTP,
  `edge` when public TLS is terminated by a proxy/load balancer,
  `self-signed-dev` for local HTTPS testing,
  and PEM cert/key files for in-process HTTPS termination
- progress notifications when the caller provides `_meta.progressToken`
- cooperative cancel propagation from `notifications/cancelled`
- Harn-native context discovery:
  package source, nearest `harn.toml`, README, and `.harn.prompt` files appear
  through `resources/list`, `resources/read`, `resources/templates/list`,
  `prompts/list`, and `prompts/get`
- `completion/complete` for file-backed prompt arguments and package/prompt
  resource template arguments
- HTTP auth hooks built on the shared `AuthPolicy` surface:
  API keys
  HMAC canonical-request signatures
  OAuth 2.1 claims injected by a hosting transport

### Site

Choose `harn serve site` when the caller is a plain HTTP client — a
browser, a webhook sender, a REST consumer — and you want the script to
answer its **own routes** rather than a fixed protocol surface.

Each routed `pub fn` receives a request dict and returns a value or an
`http_*` response envelope:

```harn
@route("POST", "/users/{id}")
pub fn update_user(req: dict) -> dict {
  return http_ok({ "id": req.path_params.id, "body": json_parse(req.body) })
}

// The `handler_*` convention needs no attribute: `handler_health` mounts
// at GET|POST /health; a bare `handler` mounts at the site root `/`.
pub fn handler_health(req: dict) -> dict {
  return http_ok({ "ok": true })
}
```

`@route("/path")` (one argument) defaults the method to `GET`;
`@route("ANY", "/path")` (or `"*"`) answers every method. A `pub fn` with
neither a `@route` attribute nor a `handler_` name stays dispatch-only —
still callable by name from the other adapters, but no bare path reaches
it.

Run it with:

```bash
harn serve site server.harn
harn serve site --bind 127.0.0.1:8788 server.harn
harn serve site --api-key "$HARN_SERVE_API_KEY" --bind 0.0.0.0:8788 server.harn
harn serve site --tls self-signed-dev server.harn
```

Mapping:

- the request dict mirrors the in-process `http_server` shape:
  `{ method, path, route, path_params, params, query, headers, body,
  body_kind, body_base64, content_length, client_ip, remote_addr }`
- `body` is set only when the payload is valid UTF-8; binary payloads set
  `body` to `nil`, `body_kind` to `base64`, and `body_base64` to the
  standard-base64 encoding of the raw bytes. Recover binary uploads with
  `bytes_from_base64(req.body_base64)` and hand the result to `multipart_parse`
- header keys are lower-cased; `client_ip` is read from
  `X-Forwarded-For` / `X-Real-IP` when present
- responses go through the same codec as every other surface, so
  `http_ok` / `http_created` / `http_not_modified` / `http_error` /
  `http_stream` / `http_sse` / `http_reply` plus content negotiation, ETag,
  and compression all behave identically. `http_reply(200, bytes, headers)`
  returns the raw bytes with caller-supplied `Content-Type` /
  `Content-Disposition` preserved.
- WebSocket handlers return
  `http_upgrade_ws(req, { on_message: "fn_name" })`; the named function
  runs per inbound frame with a `{type, data}` / `{type, data_base64}`
  message dict, and its return value is sent back (a string verbatim, any
  other value as JSON, `nil` sends nothing)
- unlike the dispatch adapters, the site host **never replay-caches** — an
  HTTP server must re-run its handler on every request

Route auth policies:

```harn
import { require_policy } from "std/harness/policy"

@scopes("resources:write")
@policy(kinds: "tenant", matches: "tenant owner")
@route("POST", "/tenants/{tenant}/resources")
pub fn update_resource(req: dict) -> dict {
  const denial = require_policy({
    kinds: ["tenant"],
    scopes: ["resources:write"],
    matches: [
      {
        kind: "tenant_mismatch",
        label: "tenant",
        left: "req.path_params.tenant",
        right: "tenant.id",
      },
      {
        kind: "resource_mismatch",
        label: "owner",
        left: "body.owner",
        right: "auth.subject",
      },
    ],
  }, req,)
  if denial != nil {
    return denial
  }
  return http_ok({ok: true})
}
```

`@scopes(...)` and `@policy(kinds: "...")` are admission gates checked before
the handler runs. `@policy(matches: "...", methods: "...")` is exported
metadata for audits; enforce those runtime comparisons with
`require_policy(...)` inside the handler. Path roots are `req`, `body` (JSON
parsed from `req.body`), `auth`, `tenant`, and optional `ctx`. Denials use
typed `details.kind` values such as `missing_auth`, `forbidden_principal_kind`,
`missing_scope`, `tenant_mismatch`, and `resource_mismatch`, and do not echo
compared tenant, subject, or resource ids.

For JSON-RPC-shaped POST bodies, select method-specific policies from
`body.method`:

```harn
import { require_policy } from "std/harness/policy"

@policy(methods: "doc.read doc.write")
@route("POST", "/rpc")
pub fn rpc(req: dict) -> dict {
  const denial = require_policy({
    method_path: "body.method",
    methods: {
      "doc.read": {scopes: ["doc:read"]},
      "doc.write": {
        kinds: ["operator"],
        scopes: ["doc:write"],
        matches: [{left: "body.owner", right: "auth.subject"}],
      },
    },
  }, req,)
  if denial != nil {
    return denial
  }
  return http_ok({ok: true})
}
```

### A2A

Choose A2A when the caller wants a peer agent rather than a bag of tools.

Typical fit:

- cross-agent delegation
- durable remote tasks
- resubscribe and callback-oriented delivery

Mapping:

- each exported `pub fn` is advertised as an A2A skill in the agent card
- inbound task text is passed to the selected exported function
- callers can select the exported function with `function`, `skillId`, or
  `message.metadata.target_agent`; if there is only one export, that export is
  selected automatically
- the shared dispatch core executes the same exported Harn function as the MCP
  adapter
- the A2A adapter owns agent cards, task lifecycle, push callbacks,
  cancellation, and resubscribe behavior

Run it with:

```bash
harn serve a2a server.harn
harn serve a2a --port 3000 server.harn
harn serve a2a --bind 0.0.0.0:3000 --api-key "$HARN_SERVE_API_KEY" server.harn
harn serve a2a --tls edge --public-url https://agent.example.com server.harn
harn serve a2a --tls self-signed-dev --port 3443 server.harn
harn serve a2a --cert certs/prod.pem --key certs/prod-key.pem server.harn
```

Behavior today:

- `harn serve a2a` binds to `127.0.0.1:8080` by default; pass `--bind
  0.0.0.0:PORT` only when the server is protected by API-key/HMAC auth and
  TLS or a trusted edge proxy
- HTTP JSON-RPC endpoint at `/`
- A2A AgentCard at `/.well-known/agent-card.json`, with compatibility aliases
  at `/.well-known/a2a-agent`, `/.well-known/agent.json`, and `/agent/card`
- A2A 0.3.0 JSON-RPC methods `message/send`, `message/stream`, `tasks/get`,
  `tasks/cancel`, `tasks/resubscribe`,
  `tasks/pushNotificationConfig/{set,get,list,delete}`, and
  `agent/getAuthenticatedExtendedCard` (returns an enriched card with
  declared security schemes and per-skill `outputSchema` to authenticated
  callers; rejects unauthenticated calls with HTTP 401 plus a
  `WWW-Authenticate` challenge, or `ExtendedAgentCardNotConfiguredError`
  / `-32007` when no `AuthPolicy` methods are configured)
- one-cycle compatibility aliases for `a2a.*`, `tasks/send`,
  `tasks/send_and_wait`, `tasks/sendSubscribe`, and `tasks/list`, with
  `Deprecation: true` on legacy HTTP responses
- RFC 8693 actor-chain metadata accepted at `message.metadata.actor_chain`;
  the adapter stores the incoming chain on task metadata and appends the served
  agent hop for the execution session
- REST-style POST paths at `/message/send`, `/message/stream`,
  `/tasks/resubscribe`, and `/tasks/cancel`, plus deprecated `/tasks/send` and
  `/tasks/send_and_wait` aliases
- `agent_progress` events stream as non-terminal `working` status updates with
  agent message text; entry lists render as markdown checklists and final task
  completion is still emitted separately
- cooperative cancel propagation into the shared VM cancel token
- push notification callbacks from caller-provided task configuration
- HTTP auth hooks built on the shared `AuthPolicy` surface:
  API keys
  HMAC canonical-request signatures
  When auth is configured, all task creation, task inspection, cancellation,
  streaming/resubscribe, and push notification config operations require it.
- optional HS256 agent-card signatures with
  `--card-signing-secret` or `HARN_SERVE_A2A_CARD_SECRET`

## TLS modes

`harn serve` HTTP adapters accept the same TLS modes exposed by the HTTP
stdlib helpers:

- `--tls plain`: bind a plain HTTP listener. Use only for loopback,
  trusted internal networks, or explicit cleartext development.
- `--tls edge`: bind a plain HTTP listener because an edge proxy,
  ingress, or load balancer terminates public TLS. The Harn layer treats the
  advertised scheme as HTTPS and emits HSTS headers. For A2A, pass
  `--public-url https://...` so agent cards point at the public edge URL.
- `--tls self-signed-dev`: generate an ephemeral self-signed certificate and
  serve HTTPS locally. This is for development only; HSTS is intentionally
  disabled so browsers are not pinned to a throwaway certificate.
- `--tls pem --cert <chain.pem> --key <key.pem>` or just `--cert ... --key ...`:
  load a PEM certificate chain and private key before the listener starts. A
  missing or invalid file is a startup failure, not a deferred request-time
  error.

Prefer edge termination for managed deployments where the platform already owns
certificate issuance, renewal, WAF/rate-limit policy, or HTTP/2/HTTP/3
negotiation. Prefer PEM termination only when the Harn process is the TLS
boundary. ALPN and SNI routing are intentionally left to edge infrastructure
until Harn has a concrete in-process need.

### ACP

Choose ACP when the caller wants a live agent session with host mediation.

Typical fit:

- editor hosts
- approval-aware local runtimes
- clients that already speak ACP session updates

Mapping:

- `harn serve acp <file.harn>` starts the packaged stdio ACP adapter
- `harn serve acp --transport websocket <file.harn>` exposes the same packaged
  adapter over a local WebSocket endpoint
- the adapter owns session state, prompt execution, permission prompts, cancel
  tokens, and bidirectional `session/update` traffic
- the `_harn/providerCatalog` extension method returns the same normalized
  provider catalog v6 artifact shape as Harn's checked-in catalog
- each `session/prompt` exposes `prompt` as the text-only prompt string,
  `runtime_prompt_content()` from `std/runtime` as typed, normalized Harn
  content blocks. The legacy `prompt_content` and `prompt_messages` globals
  remain available during the downstream migration window.

## Design rule

If a behavior changes depending on whether the caller arrived over MCP, A2A, or
ACP, first ask whether it belongs in the adapter or in the shared core.

It belongs in the shared core when it affects:

- what function is invoked
- how arguments are normalized
- auth decisions
- replay semantics
- cancellation behavior
- observability or trust records

It belongs in the adapter when it affects:

- wire format
- discovery documents or cards
- streaming shape
- session lifecycle
- protocol-specific error envelopes
