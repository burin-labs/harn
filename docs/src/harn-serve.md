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
harn serve api --default-session-mode code agent.harn
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

The native Agents API serves one shared local workspace. Every accepted
credential authorizes that whole workspace, including the canonical session-store
routes. A key's subject namespaces retry bindings; it does not make tasks or
sessions private to their creator. Tenant-bound credentials and credentials with
scoped grants return `403 unsupported_authorization_policy`. Use a hosted or site
authorization owner when you need tenant, scope, or resource restrictions. Health,
version, OpenAPI, and the API root remain public discovery routes.

New sessions use ACP `ask` mode unless the server was started with
`--default-session-mode` or the create request supplies `mode_id`. The accepted
values are `ask`, `architect`, `code`, and `shadow`; an invalid value is rejected
before a session is created. The session response reports the effective
`mode_id`. `code` allows the pipeline's internal state writes and coding
effects, so use it only for a server whose callers are authorized to run those
effects. Selecting a mode does not supply an embedder for product-specific
`host/call` operations; those pipelines still need their host adapter.

Requests use `Harn-Agents-Protocol-Version: agents-protocol-2026-10-11`.
An absent or unsupported version returns `426` with
`unsupported_protocol_version`; public discovery does not require the header.

Task submission accepts `Idempotency-Key` on `POST /v1/tasks` and
`POST /v1/sessions/{session_id}/tasks`. Use the same key when retrying the same
accepted JSON request. Both routes resolve to the same session target and
return the original task ID. A changed request returns `409` with
`idempotency_key_reused`. Other local API routes reject this header with
`idempotency_not_supported`.

Keys contain 1 to 255 printable ASCII bytes after surrounding spaces are
trimmed. Each binding belongs to the authenticated principal, workspace, and
session. API keys have distinct, stable private identities; embedders can set
`ApiKeyEntry::with_subject` to keep an account identity across key rotation.
Admission persists the task, retry binding, and initial control events in one
transaction. Bindings last 24 hours from creation and remain bound while the
task is active. A retry does not renew that expiry.

File-backed and inline tasks stay `SUBMITTED` during source and VM preparation. Prompt
admission publishes its execution owner, changes it to `WORKING`, and sets
`started_at`. Cancellation during session restore or before prompt
admission prevents execution. Ordinary session messages with `run: true`
continue to run without a Task. Approval requests use `AUTH_REQUIRED`; questions use
`INPUT_REQUIRED`. Failed answer delivery restores the previous waiting state.
Task-owned approval, progress, and output events retain their admitted task ID
when another task queues in the same session. Session-wide events remain taskless.
Approval answers reach a waiting task while its prompt is suspended; they do not
wait behind the prompt they must resume.
Task input is recorded with its generated message ID before it enters the
session input queue. Storage failure returns `503` without queueing the input;
an accepted input remains recorded and published if its HTTP caller disconnects.
Input addressed to a queued task returns `409`: steering belongs to the task
currently executing in that session. Undelivered task inputs are revoked when
its prompt ends, so they cannot reach the next task. Ordinary session-scoped ACP
inputs continue across prompt replacement.
Task cancellation selects only that task's admitted prompt. Canceling a queued
task doesn't stop the running task, and canceling the running task doesn't stop
queued tasks from starting. Untargeted ACP session cancellation still stops all
prepared prompts in the session.

Tasks and their control history survive a server restart. An unfinished task
whose execution lease is no longer held becomes `FAILED` with
`task_interrupted`; retrying its key reads that result without dispatching the
prompt again. A task still running in another server is left active. This
prevents duplicate task dispatch, but does not promise exactly-once external
effects after a process interruption. Session transcripts retain their
existing workspace store.
Sessions attached to accepted tasks remain readable and mutable after restart.
Task execution and session controls restore the same ACP session and API
projection. Metadata, summary, and model-policy updates remain in the existing
session descriptor. A failed update restores the previous policy or discards the
live projection so the next control must restore the stored configuration.
Accepted forks retain their parent, root, and branch identity across restart,
including when another fork descends from a restored session. A fork returns
its own transcript identity and does not inherit its parent's event cursor.
Creation validates object `metadata` and a string or null `persona_id`. Fork requests
require a string or null `branch_id` and object `metadata`; malformed bodies
fail before creating a child.
Truncation retains the requested leading messages in the canonical transcript,
including across restart and the next provider call. It refuses a still-active
transcript writer or a failed store write; the earlier audit events remain stored.
Closing a session durably cancels
its unfinished tasks; new task submissions return `409 session_closed`.

Disconnecting the HTTP client doesn't withdraw a permission answer queued for
delivery. HITL answers remain in flight until the runtime acknowledges them.
Conflicting answers return `409` while delivery is pending or after the request
is answered. Failed delivery leaves the request pending.

Native workspace roots name existing directories inside the configured server
workspace. Relative roots resolve against that workspace, and registration stores
the canonical path. Missing directories, file roots, parent components and symlinks
outside the server workspace return `400 invalid_workspace_root`.
Discovery
advertises `/v1/runtime`, whose `workspace_root` reports the served directory.
The Agents conformance runner uses that root when creating its probe workspace
and session, including when only the idempotency category is selected. A
logical URI such as `harn://conformance/...` does not create a native directory.

Events include the resource pointer and required envelope fields in history and
SSE streams. Their sequence is the existing durable event allocation number;
gaps are permitted. Task control events retain their sequence on replay after
restart. Live notifications and standalone non-task history stay in the serving
process; the native discovery response advertises no replay capability.

SSE reconnects accept `Last-Event-ID`; the existing `after` and `cursor` query
parameters remain supported. A known cursor resumes after that event. Conflicting,
unknown, or unavailable cursors emit `error` with `cursor_unavailable` and close
the stream. Close the client stream when handling this terminal error; automatic
reconnection cannot recover unavailable history.

Sessions default to the configured workspace when `workspace_id` is omitted.
An explicit ID must name a registered workspace. An unknown ID returns `404`
before a session is opened; invalid identity types or blank IDs return `400`.

Permission responses stay pending until the runtime accepts the answer. If
task storage or response delivery fails, the request remains available to
retry. Concurrent answers, repeated answers, and answers for terminal tasks
return `409`; a successfully acknowledged answer cannot be answered again.

Session create and update requests accept an optional typed `model_policy`:

```json
{
  "model_policy": {
    "provider": "openai",
    "model": "gpt-6-sol",
    "reasoning_effort": "high"
  }
}
```

`provider` must name a registered provider, `model` is its provider-native
model identifier, and `reasoning_effort` is optional. The supported efforts are
`none`, `minimal`, `low`, `medium`, `high`, `xhigh`, and `max`. A policy set on
create or update is returned by session get/list, inherited by forks, and used
by subsequent session execution. Omitting `model_policy` preserves the current
policy and existing defaults; sending `null` on update clears it.

Precedence is explicit per-call provider/model/reasoning options, then the
session `model_policy`, then persona or script policy, then ambient runtime
defaults. This keeps task-specific calls authoritative while giving
catalog-aware hosts a typed session default.

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
- stable Streamable HTTP `POST` endpoint at `--path`
- `tools/list` includes `outputSchema` from the resolved Harn return type and
  `_meta["com.harnlang/toolContract"].errorSchema` from the resolved `throws`
  type.
  Imported aliases, structs, enums, optional fields, lists, typed maps, unions,
  and generic instantiations keep their structure. `inputSchema` continues to
  project caller-owned parameters, inline records, and structural aliases.
- `tools/call` returns object-shaped results in `structuredContent`. The value
  satisfies the same `outputSchema` advertised by `tools/list`.
- A value matching a public callable's declared `throws` type returns
  `isError: true` with typed data at
  `_meta["com.harnlang/toolContract"].applicationError`. It never appears in
  success `structuredContent` or as a JSON-RPC transport error.
- record and enum results are direct objects. Scalar, list, and nullable
  top-level results use `{ "result": value }` because MCP structured content
  and output schemas are object-valued contracts.
- exported enum values use a discriminated object with `enum`, `variant`, and
  positional `fields` keys. Each field schema has the declared parameter name
  as its JSON Schema `title`. SDK generators don't need to parse enum display
  text or Harn source.
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

An exported enum result has this wire shape:

```json
{
  "enum": "Outcome",
  "variant": "Success",
  "fields": [{ "message": "hello Ada", "tags": ["typed"] }]
}
```

The corresponding `outputSchema` is a discriminated `oneOf`. Generic payloads
are instantiated before projection, so `Outcome<Greeting>` contains the full
`Greeting` field schema rather than an open object.

### Site

Choose `harn serve site` when the caller is a plain HTTP client — a
browser, a webhook sender, a REST consumer — and you want the script to
answer its **own routes** rather than a fixed protocol surface.

Each routed `pub fn` receives a request dict and returns a value or an
`http_*` response envelope:

```harn
@route("POST", "/users/{id}")
pub fn update_user(req: dict) -> dict {
  return http_ok({
    "id": req.path_params.id, "body": json_parse(req.body),
  })
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

For a local tool, let the operating system choose a free port and publish its
address to a new file:

```bash
harn serve site --bind 127.0.0.1:0 --ready-file /tmp/site-ready.json server.harn
```

The JSON receipt contains `schema_version: 1`, `pid`, and `url`. Its URL uses
the actual listener address, even when `--public-url` advertises another URL.
The complete receipt appears after routing, binding, and TLS setup succeed. Startup fails
if the file already exists or cannot be written. Use a fresh path for each run.
The receipt records startup; check the process or make a request for current health.

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
pub fn update_resource(harness: Harness, req: dict) -> dict {
  const denial = require_policy(harness.auth, harness.tenant, {
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
pub fn rpc(harness: Harness, req: dict) -> dict {
  const denial = require_policy(harness.auth, harness.tenant, {
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
- declared application failures terminate the task as `failed` and preserve
  typed data at `task.metadata.harn.applicationError`; human history and status
  text use a generic application-error summary and never read application data
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
  provider catalog v8 artifact shape as Harn's checked-in catalog
- each `session/prompt` exposes `prompt` as the text-only prompt string,
  `runtime_prompt_content()` from `std/runtime` as typed, normalized Harn
  content blocks. The legacy `prompt_content` and `prompt_messages` globals
  remain available during the downstream migration window.

Clients declare supported standard operations in `initialize.clientCapabilities`.
Set `terminal: true` only when the client implements the complete `terminal/*`
lifecycle. Filesystem operations use `fs.readTextFile` and `fs.writeTextFile`.
Omitted, false or malformed terminal and filesystem capabilities do not permit those operations;
the server refuses an unavailable terminal rather than forwarding a command to it.
The `host/call` extension retains its separate host-capability contract.

Session requests may supply an empty `mcpServers` list. Nonempty lists are
refused until session MCP intake is implemented; configure servers through the
runtime's existing MCP configuration. HTTP and SSE session intake are not
advertised. A client `systemPrompt` is likewise refused rather than silently
ignored. These refusals happen before creating or restoring a session.

`session/new`, `session/load`, and `session/resume` accept
`additionalDirectories` as the complete ordered list of additional workspace
roots. Paths must name existing absolute directories. A malformed entry or a
root outside process confinement refuses the whole request. The list expands
filesystem scope without changing `cwd`; it cannot change during an active
prompt. Existing execution policy ceilings still apply.

Omitting the list or supplying `[]` activates no additional roots, including
when loading a stored session. Stored paths report prior state and do not
authorize a new launch. Accepted replacements persist before changing live
grants; `session/list` reports the canonical ordered list. Open client sessions
retain their runtime state until closed or released. At capacity, admission
fails rather than evicting a still-open client's roots.

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
