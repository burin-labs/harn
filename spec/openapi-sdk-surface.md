# Harn SDK OpenAPI surface audit

`spec/openapi.yaml` is the canonical source for generated Harn Agents API SDKs.
It covers the public REST and server-sent event surface exposed by
`harn serve api`, including the stable local discovery paths and `/v1`
operations that SDKs can model as normal request/response operations.

## Included

- Protocol discovery and public agent-card endpoints.
- Local server discovery and status endpoints:
  `/openapi.json`, `/health`, `/version`, `/v1/runtime`, `/v1/capabilities`,
  and `/v1/tools`.
- Personas, workspaces, sessions, tasks, branches, messages, artifacts, events,
  receipts, memories, vaults, connectors, skills, outcomes, and quotas.
- Local permission request inspection and response endpoints that forward ACP
  `session/request_permission` and Harn HITL decisions through the runtime.
- SSE entry points modeled as HTTP `GET` operations:
  `/v1/events/stream`, `/v1/sessions/{session_id}/events/stream`, and
  `/v1/tasks/{task_id}/stream`.

## Deliberately outside this OpenAPI document

- `harn mcp serve --transport http` exposes MCP Streamable HTTP JSON-RPC at
  its configured path, default `/mcp`, plus optional legacy SSE/message paths.
  MCP clients should use MCP protocol tooling rather than the Agents REST SDK.
- `harn serve a2a` exposes A2A JSON-RPC plus
  `/.well-known/agent-card.json`. A2A has its own protocol schema and
  conformance fixtures.
- `harn orchestrator serve` can mount ACP over WebSocket at `/acp`. ACP
  messages are JSON-RPC frames over WebSocket and are documented separately in
  `docs/src/acp/websocket.md`.
- Portal and local orchestrator admin endpoints are implementation/admin
  surfaces, not the stable public Agents SDK contract.

## Generation gate

`scripts/generate_sdk_clients.sh` regenerates both clients from the same
OpenAPI document:

```sh
scripts/generate_sdk_clients.sh --language all --output-dir target/generated-sdks
```

`.github/workflows/sdk-codegen.yml` runs this generator matrix on PRs that
touch the spec or generator, on `main`, through the merge queue, and for every
published release. Release runs upload generated Python and TypeScript client
artifacts for `burin-labs/harn-sdk-python` and
`burin-labs/harn-sdk-typescript` to consume.

The generated artifact manifest includes both the exact Harn release version
and the SDK package version. SDK package versions follow Harn's minor line:
Harn `X.Y.Z` produces SDK version `X.Y.0`.
