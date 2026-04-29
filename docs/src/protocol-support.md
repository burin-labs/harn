# Protocol Support Matrix

This page is the quick routing table for Harn's protocol surfaces. The
canonical task guides remain:

- [MCP, ACP, and A2A integration](./mcp-and-acp.md) for user-facing protocol
  usage.
- [Outbound workflow server](./harn-serve.md) for the shared serving core used
  by `harn serve mcp`, `harn serve a2a`, and `harn serve acp`.
- [Orchestrator MCP server](./mcp-server.md) for controlling a local
  orchestrator over MCP.
- [Bridge protocol](./bridge-protocol.md) for host-mediated tool execution
  underneath ACP.

| Protocol surface | Harn role | Entry point | Transports | Discovery | Auth and control notes |
|---|---|---|---|---|---|
| MCP client | Connect from Harn code to external MCP servers. | `mcp_connect(...)`, `[[mcp]]` in `harn.toml`, `harn mcp login` for remote OAuth state. | stdio and remote HTTP. | Optional Server Cards through `mcp_server_card(...)` and `card = ...` config. | Lazy boot, ref-counted release, skill-scoped binding, OAuth token reuse, and tool-search indexing are covered in [MCP client](./mcp-and-acp.md#mcp-client-connecting-to-mcp-servers). |
| MCP server for a Harn module | Expose exported `pub fn` functions or registered Harn tools/resources/prompts as MCP tools. | `harn serve mcp <file.harn>` | stdio, Streamable HTTP, legacy SSE compatibility endpoints. | Optional published Server Card through `--card`. | Uses the shared `harn-serve` dispatch core. See [MCP server](./mcp-and-acp.md#mcp-server-exposing-harn-as-an-mcp-server) and [Outbound workflow server](./harn-serve.md#mcp). |
| Orchestrator MCP server | Let MCP clients fire triggers, inspect queues, retry DLQ entries, and read orchestrator state. | `harn mcp serve --config ./harn.toml --state-dir ./.harn/orchestrator` | stdio and HTTP. | MCP tool and resource catalog described on the page. | Optional API keys through `HARN_ORCHESTRATOR_API_KEYS`; HTTP accepts bearer or `x-api-key`. See [Orchestrator MCP server](./mcp-server.md). |
| ACP stdio | Run Harn as an ACP backend for editor and local hosts. | `harn serve acp <file.harn>` | stdio JSON-RPC. | ACP `initialize` capability negotiation. | ACP owns session lifecycle while the [Bridge protocol](./bridge-protocol.md) keeps concrete tool execution under host control. See [ACP](./mcp-and-acp.md#acp-agent-client-protocol). |
| ACP WebSocket | Expose ACP sessions from the orchestrator HTTP listener. | `harn orchestrator serve ...` with the `/acp` endpoint. | WebSocket text frames carrying one ACP JSON-RPC message each. | Same ACP method surface as stdio after connection. | Requires bearer auth on the orchestrator endpoint. See [ACP over WebSocket](./acp/websocket.md). |
| A2A server | Expose a Harn module as a peer-agent endpoint. | `harn serve a2a <file.harn>` | HTTP JSON-RPC, SSE task streaming, REST-style task aliases. | Agent cards at `/.well-known/a2a-agent` plus compatibility aliases. | Implements A2A 0.3.0 methods (`message/send`, `message/stream`, `tasks/*`, push notification config, authenticated extended card) with one-cycle deprecation headers for legacy aliases. See [A2A](./mcp-and-acp.md#a2a-agent-to-agent-protocol). |
| A2A push connector | Receive A2A push notifications as orchestrator trigger events. | `kind = "a2a-push"` trigger manifest entries. | HTTP webhook ingress. | Trigger manifest and connector catalog. | Supports JWT/JWKS verification when `[triggers.a2a_push]` is configured; legacy routes can use bearer or HMAC auth. See [A2A push connector](./connectors/a2a-push.md). |

## Canonical ownership

Use [MCP, ACP, and A2A integration](./mcp-and-acp.md) when you need examples
or user-facing protocol behavior. Use the narrower pages when you need the
operational details for one host surface:

- [Orchestrator MCP server](./mcp-server.md) owns the `harn mcp serve` control
  plane.
- [ACP over WebSocket](./acp/websocket.md) owns the orchestrator's browser and
  remote-IDE ACP transport.
- [Outbound workflow server](./harn-serve.md) owns shared adapter mechanics and
  the "which adapter should I choose?" decision.
- [Bridge protocol](./bridge-protocol.md) owns host bridge wire details and
  tool-gate semantics.
