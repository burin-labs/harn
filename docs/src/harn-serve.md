# Outbound Workflow Server

`harn-serve` is the shared outbound-server crate for exposing Harn workflows to
external callers. It contains the MCP, A2A, and ACP adapters plus the shared
dispatch, auth, replay, and export-catalog pieces those adapters use.

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

## Picking an adapter

Choose the adapter based on the caller's mental model, not by protocol
popularity alone.

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
- HTTP auth hooks built on the shared `AuthPolicy` surface:
  API keys
  HMAC canonical-request signatures
  OAuth 2.1 claims injected by a hosting transport

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
harn serve a2a --tls edge --public-url https://agent.example.com server.harn
harn serve a2a --tls self-signed-dev --port 3443 server.harn
harn serve a2a --cert certs/prod.pem --key certs/prod-key.pem server.harn
```

Behavior today:

- HTTP JSON-RPC endpoint at `/`
- A2A AgentCard at `/.well-known/agent-card.json`, with compatibility aliases
  at `/.well-known/a2a-agent`, `/.well-known/agent.json`, and `/agent/card`
- `a2a.SendMessage`, `a2a.SendStreamingMessage`, `a2a.GetTask`,
  `a2a.CancelTask`, and `a2a.ListTasks`
- A2A task aliases `tasks/send`, `tasks/send_and_wait`, `tasks/resubscribe`,
  `tasks/cancel`, and `tasks/list`
- REST-style POST aliases at `/tasks/send`, `/tasks/send_and_wait`,
  `/tasks/resubscribe`, and `/tasks/cancel`
- cooperative cancel propagation into the shared VM cancel token
- push notification callbacks from caller-provided task configuration
- HTTP auth hooks built on the shared `AuthPolicy` surface:
  API keys
  HMAC canonical-request signatures
- optional HS256 agent-card signatures with
  `--card-signing-secret` or `HARN_SERVE_A2A_CARD_SECRET`

## TLS Modes

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
- the adapter owns session state, prompt execution, permission prompts, cancel
  tokens, and bidirectional `session/update` traffic

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
