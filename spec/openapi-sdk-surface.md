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
published release. A required `Both SDK language artifacts` job fail-closes
when either language is missing. Published releases also attach both clients
as durable GitHub release assets for `burin-labs/harn-sdk-python` and
`burin-labs/harn-sdk-typescript`.

The generator pins its complete toolchain, including the TypeScript compiler
peer used by `@hey-api/openapi-ts`. The generated artifact manifest records the
toolchain versions alongside the exact Harn release and SDK package versions.
SDK package versions follow Harn's minor line: Harn `X.Y.Z` produces SDK
version `X.Y.0`.

## Retrieval

Downstream CI pins a released Harn tag. Do not generate clients from Harn
`main`.

```sh
gh release download vX.Y.Z \
  --repo burin-labs/harn \
  --pattern 'harn-sdk-*.tar.gz' \
  --dir generated-sdks
```

Each archive is the generated client tree plus `harn-sdk-generation.txt`.
That manifest always records `harn_version` and `openapi_sha256`.

```sh
tar -tzf generated-sdks/harn-sdk-typescript.tar.gz | grep harn-sdk-generation.txt
tar -xOf generated-sdks/harn-sdk-typescript.tar.gz ./harn-sdk-generation.txt
```

`scripts/check_sdk_release_artifacts.sh --release vX.Y.Z` fails if either
`harn-sdk-python.tar.gz` or `harn-sdk-typescript.tar.gz` is absent from the
GitHub release. The same check accepts a local `--dir` of extracted
`python/` and `typescript/` trees.

Workflow-run artifacts named `harn-sdk-<language>-<sha>` remain available
for 30 days as a recovery path. They are not the pin: the GitHub release
assets named above survive after the workflow-run artifacts expire.
