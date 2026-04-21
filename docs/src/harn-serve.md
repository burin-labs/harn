# Outbound Workflow Server

`harn-serve` is the shared outbound-server core for exposing Harn workflow
entrypoints to external callers. It is the common layer under the planned MCP,
A2A, and ACP adapters from issue `#301`.

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
- a transport-adapter trait so MCP, A2A, and ACP can layer their own wire
  protocol on top without redefining dispatch semantics

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
```

Behavior today:

- stdio transport for local subprocess-style MCP clients
- Streamable HTTP `POST` / `GET` endpoint at `--path`
- legacy SSE compatibility endpoints at `--sse-path` and `--messages-path`
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

- the shared dispatch core executes the same exported Harn function
- the A2A adapter owns agent cards, task lifecycle, and resubscribe behavior

### ACP

Choose ACP when the caller wants a live agent session with host mediation.

Typical fit:

- editor hosts
- approval-aware local runtimes
- clients that already speak ACP session updates

Mapping:

- the shared dispatch core still owns function loading, auth metadata,
  cancellation, and trust/trace emission
- the ACP adapter owns session state, permission prompts, and bidirectional
  updates

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
