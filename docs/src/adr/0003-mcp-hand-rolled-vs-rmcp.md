# ADR 0003: use the official Rust MCP SDK

## Status

Superseded on 2026-08-02. Harn now uses `rmcp` 3.1 for MCP client lifecycle,
stdio transport, framing, request association, standard metadata, version
negotiation, and typed protocol errors.

The previous decision kept a hand-written implementation because `rmcp` 1.7
did not model the proposed 2026 protocol. The revisit condition recorded by
that decision has occurred: MCP 2026-07-28 is stable, and `rmcp` 3.1 supports
it together with the released 2025 and 2024 protocol versions.

## Decision

The official SDK owns commodity MCP protocol behavior:

- `server/discover` and legacy `initialize` lifecycle selection;
- protocol-version negotiation for 2026-07-28, 2025-11-25, 2025-06-18,
  2025-03-26, and 2024-11-05;
- JSON-RPC framing, IDs, response association, cancellation, and bounded
  shutdown for child-process clients;
- stable error codes, method names, capabilities, request metadata, standard
  HTTP headers, MRTR types, and task types.

Harn continues to own product policy and host integration:

- VM values and builtin return shapes;
- egress and SSRF policy, OAuth state, capability fixtures, and audit records;
- roots, elicitation, sampling, and progress routing;
- workflow dispatch, task execution, authorization, and server persistence.

The boundary is the SDK handler and transport adapter, not a second protocol
model. Harn constants that must be emitted into generated bindings are checked
against `rmcp`'s typed registry.

## Compatibility

The default client uses the stable discovery lifecycle and prefers
2026-07-28. It negotiates another SDK-supported released version when a
discovery-capable peer requires one. When a peer proves it is legacy by
returning `Method not found` for `server/discover`, the SDK performs the
2025-11-25 initialize/initialized handshake.

The former `rc` and explicit legacy modes were removed. Compatibility fallback
is available only where the SDK provides it as part of its standard automatic
lifecycle.

## Consequences

The raw stdio client framing, request-ID loop, response drain, lifecycle probe,
and child shutdown code have been deleted. Stable task names and result shapes
replace the draft `tasks/list`, `tasks/result`, nested task envelope, and
`execution.taskSupport` fields.

Harn's HTTP client and server adapters still contain Harn-owned authorization,
request streaming, and dispatch policy. They use the SDK registry as their
protocol oracle; moving more of those adapters behind SDK transports is
appropriate only when it preserves Harn's egress, OAuth, audit, cancellation,
and streaming contracts without duplicating them.

## Evidence

- `protocol_registry_matches_official_sdk` checks Harn's public version and
  error-code projections against `rmcp`.
- The MCP compatibility suite exercises stable discovery, standard headers,
  MRTR, task polling, and both Harn server surfaces; SDK-owned tests exercise
  released-version negotiation and the older stdio lifecycle.
- The canonical Harn stdio client test runs discovery over a real child-process
  transport owned by `rmcp`; released-version fallback remains covered by the
  SDK that implements it.

See the [MCP and ACP integration reference](../mcp-and-acp.md) for the public
configuration contract.
