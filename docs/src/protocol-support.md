# Protocol support matrix

This page is the quick routing table for Harn's protocol surfaces. The
canonical task guides remain:

- [MCP, ACP, and A2A integration](./mcp-and-acp.md) for user-facing protocol
  usage.
- [Outbound workflow server](./harn-serve.md) for the shared serving core used
  by `harn serve api`, `harn serve mcp`, `harn serve a2a`, and
  `harn serve acp`.
- [Orchestrator MCP server](./mcp-server.md) for controlling a local
  orchestrator over MCP.
- [Bridge protocol](./bridge-protocol.md) for host-mediated tool execution
  underneath ACP.

| Protocol surface | Harn role | Entry point | Transports | Discovery | Auth and control notes |
|---|---|---|---|---|---|
| MCP client | Connect from Harn code to external MCP servers. | `mcp_connect(...)`, `[[mcp]]` in `harn.toml`, `harn mcp login` for remote OAuth state. | stdio and remote HTTP. | Optional Server Cards through `mcp_server_card(...)` and `card = ...` config. | Lazy boot, ref-counted release, skill-scoped binding, OAuth token reuse, and tool-search indexing are covered in [MCP client](./mcp-and-acp.md#mcp-client-connecting-to-mcp-servers). |
| MCP server for a Harn module | Expose exported `pub fn` functions or registered Harn tools/resources/prompts as MCP tools. | `harn serve mcp <file.harn>` | stdio, Streamable HTTP, legacy SSE compatibility endpoints. | Optional published Server Card through `--card`. | Uses the shared `harn-serve` dispatch core. See [MCP server](./mcp-and-acp.md#mcp-server-exposing-harn-as-an-mcp-server) and [Outbound workflow server](./harn-serve.md#mcp). |
| Orchestrator MCP server | Let MCP clients fire triggers, inspect queues, retry DLQ entries, and read orchestrator state. | `harn mcp serve --config ./harn.toml --state-dir ./.harn/orchestrator` | stdio and HTTP. | MCP tool and resource catalog plus RFC 9728 OAuth protected-resource metadata when OAuth is configured. | HTTP supports OAuth resource-server mode via `HARN_MCP_OAUTH_*`; legacy API keys remain available through `HARN_ORCHESTRATOR_API_KEYS`. See [Orchestrator MCP server](./mcp-server.md). |
| Local Agents API | Expose a `.harn` agent to SDKs through OpenAPI-described HTTP and SSE. | `harn serve api <file.harn>` | HTTP JSON plus Server-Sent Events. | `GET /openapi.json`, `/health`, `/version`, `/v1/runtime`, `/v1/capabilities`, and `/v1/tools`. | Uses the ACP session runtime internally for sessions, prompts, cancellation, permissions, HITL, transcript, EventLog, and replay visibility. Optional API-key/HMAC auth and shared TLS listener modes are available. See [Outbound workflow server](./harn-serve.md#local-agents-api). |
| ACP stdio | Run Harn as an ACP backend for editor and local hosts. | `harn serve acp <file.harn>` | stdio JSON-RPC. | ACP `initialize` capability negotiation. | ACP owns session lifecycle while the [Bridge protocol](./bridge-protocol.md) keeps concrete tool execution under host control. See [ACP](./mcp-and-acp.md#acp-agent-client-protocol). |
| ACP WebSocket | Expose ACP sessions to editors or remote hosts. | `harn serve acp --transport websocket <file.harn>` for a direct endpoint; `harn orchestrator serve ...` for the retained `/acp` hub. | WebSocket text frames carrying one ACP JSON-RPC message each. | Same ACP method surface as stdio after connection. | Direct serve can use in-band ACP `authenticate` or upgrade-time API keys; the orchestrator hub requires bearer auth when configured. See [ACP over WebSocket](./acp/websocket.md). |
| A2A server | Expose a Harn module as a peer-agent endpoint. | `harn serve a2a <file.harn>` | HTTP JSON-RPC, canonical HTTP+JSON/REST under `/v1`, SSE task streaming, plus deprecated REST-style aliases. | Agent cards at `/.well-known/a2a-agent` plus compatibility aliases; JSON-RPC and HTTP+JSON transports advertised via `additionalInterfaces`. | Implements A2A 0.3.0 methods (`message/send`, `message/stream`, `tasks/*`, push notification config, authenticated extended card) over both transports, with one-cycle deprecation headers for legacy paths. See [A2A](./mcp-and-acp.md#a2a-agent-to-agent-protocol). |
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
