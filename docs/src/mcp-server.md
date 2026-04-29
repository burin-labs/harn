# Orchestrator MCP Server

`harn mcp serve` exposes a local Harn orchestrator as an MCP server so any MCP
client can fire triggers, inspect queues, replay events, and read runtime state
without a Harn-specific adapter.

This page is the canonical reference for the orchestrator control-plane MCP
server. For the general MCP client/server guide, see
[MCP, ACP, and A2A integration](./mcp-and-acp.md); for the full protocol routing
table, see [Protocol support matrix](./protocol-support.md).

The server is aimed at closed-loop agent clients that already know how to speak
MCP, including:

- Cursor Composer
- Claude Desktop
- Claude Code
- LangChain MCP adapters

## Quickstart

Hook Harn into Cursor Composer in 3 steps:

1. Start the server from the workspace that owns your orchestrator manifest.

```bash
harn mcp serve --config ./harn.toml --state-dir ./.harn/orchestrator
```

1. Point Cursor at the command as a stdio MCP server.

```json
{
  "mcpServers": {
    "harn": {
      "command": "harn",
      "args": ["mcp", "serve", "--config", "/absolute/path/to/harn.toml", "--state-dir", "/absolute/path/to/.harn/orchestrator"]
    }
  }
}
```

1. Ask the client to call Harn tools such as `harn.trigger.list` or
   `harn.orchestrator.inspect`.

Example prompts:

- "List the Harn triggers in this workspace."
- "Fire the `cron-ok` trigger with an empty payload."
- "Show the Harn DLQ and retry the newest entry."
- "Scan this diff for secrets before I open a PR."

## Transports

`harn mcp serve` supports:

- `stdio` for local spawned clients. This is the default.
- `http` for remote MCP clients.

HTTP mode exposes:

- Streamable HTTP POST, GET, and DELETE at `--path` (default `/mcp`)
- Legacy SSE GET at `--sse-path` (default `/sse`), marked with
  `Deprecation: true`
- Legacy SSE POST at `--messages-path` (default `/messages`), marked with
  `Deprecation: true`

Streamable HTTP clients initialize with `POST /mcp`. The initialize response
includes an `Mcp-Session-Id` header, and clients must echo that header on later
POST, GET, and DELETE requests. Requests normally return one
`application/json` JSON-RPC response; clients that only accept
`text/event-stream` receive an SSE stream containing `message` events. `GET
/mcp` opens the server-to-client SSE stream for notifications, and `DELETE
/mcp` terminates the session.

Example:

```bash
harn mcp serve \
  --config ./harn.toml \
  --state-dir ./.harn/orchestrator \
  --transport http \
  --bind 127.0.0.1:8765
```

## Auth

Set `HARN_ORCHESTRATOR_API_KEYS` to a comma-separated key list to require API
keys.

HTTP clients can authenticate with either:

- `Authorization: Bearer <key>`
- `x-api-key: <key>`

Stdio clients authenticate during `initialize` using a Harn extension field:

```json
{
  "capabilities": {
    "harn": {
      "apiKey": "test-key"
    }
  }
}
```

If `HARN_ORCHESTRATOR_API_KEYS` is unset, the MCP server runs without auth.

## Tool Catalog

### `harn.secret_scan`

Scans arbitrary text or diffs for high-signal leaked credentials and returns a
redacted finding list. Use it before commit or PR-open flows. The server also
accepts the legacy alias `harn::secret_scan`.

Input:

```json
{
  "content": "token = \"ghp_example...\""
}
```

Returns a JSON array of findings. Each finding includes:

- `detector`
- `source`
- `title`
- `line`
- `column_start`
- `column_end`
- `start_offset`
- `end_offset`
- `redacted`
- `fingerprint`

### `harn.trigger.fire`

Dispatch a trigger inline.

Input:

```json
{
  "trigger_id": "cron-ok",
  "payload": {}
}
```

Returns the dispatch handle summary including `event_id` and `status`.

### `harn.trigger.list`

Lists manifest-backed triggers with:

- `trigger_id`
- `kind`
- `provider`
- `when`
- `handler`
- `version`
- `state`
- `metrics`

### `harn.trigger.replay`

Replays a historical event. Supports `as_of` to resolve bindings against a
historical timestamp when needed.

```json
{
  "event_id": "trigger_evt_123",
  "as_of": "2026-04-19T18:00:00Z"
}
```

### `harn.orchestrator.queue`

Returns queue counts plus recent head previews for:

- inbox
- outbox
- attempts
- DLQ

### `harn.orchestrator.dlq.list`

Lists pending dead-letter entries.

### `harn.orchestrator.dlq.retry`

Retries one DLQ entry by id.

```json
{
  "entry_id": "dlq_123"
}
```

### `harn.orchestrator.inspect`

Returns the dispatcher snapshot, trigger-centric inspect data, persisted
orchestrator snapshot, flow-control state, and recent dispatch records.

### `harn.trust.query`

Placeholder trust-graph surface. Today it returns:

```json
{
  "results": []
}
```

## Resources

The server exposes these MCP resources:

- `harn://manifest`
- `harn://event/<event_id>`
- `harn://dlq/<entry_id>`

`harn://event/<event_id>` includes the recorded trigger event plus related
outbox/attempt/DLQ/action-graph trace entries.

## Prompts

The server exposes `.harn.prompt` files from the project root and from
installed prompt-library packages under `.harn/packages/<alias>`.
TOML front matter can define display metadata and MCP arguments:

```harn,ignore
---
id = "review"
description = "Review code"
[[arguments]]
name = "code"
description = "Code to review"
required = true
---
Review this:
{{ code }}
```

`prompts/get` renders the template with the supplied `arguments` object.
The server advertises `prompts.listChanged = true` and emits
`notifications/prompts/list_changed` when watched `.harn.prompt` files or
package metadata changes.

## Protocol Support

`harn mcp serve` negotiates MCP protocol version `2025-11-25`. It is a
control-plane server for Harn orchestration state, so it supports tools,
resources, prompts, logging, cancellation, progress, and streamable HTTP
sessions. It does not expose completions, resource subscriptions, roots,
sampling, elicitation, or MCP tasks.

| Method or feature | Status |
|---|---|
| `initialize`, `notifications/initialized`, `ping` | Supported |
| `logging/setLevel` | Accepted |
| `tools/list`, `tools/call` | Supported for the Harn tool catalog above |
| `notifications/progress`, `notifications/cancelled` | Supported for cancellable work |
| `resources/list`, `resources/read` | Supported for manifest, event, and DLQ resources |
| `resources/templates/list` | Supported; returns an empty list |
| `prompts/list` | Supported for `.harn.prompt` files in the project and prompt-library packages |
| `prompts/get` | Supported; renders prompt templates with supplied arguments |
| `completion/complete` | Explicitly unsupported |
| `resources/subscribe`, `resources/unsubscribe` | Explicitly unsupported |
| `roots/list` | Explicitly unsupported |
| `sampling/createMessage` | Explicitly unsupported |
| `elicitation/create` | Explicitly unsupported |
| `tasks/get`, `tasks/result`, `tasks/list`, `tasks/cancel` | Explicitly unsupported |
| `tools/call` with `params.task` | Rejected; task-augmented execution is not advertised |

Explicitly unsupported methods return a JSON-RPC error with code `-32601` and
`error.data.type = "mcp.unsupportedFeature"`. Tool calls that request
task-augmented execution return `-32602` because the request parameters ask for
a capability Harn does not advertise.

## Observability

Every MCP tool call appends an `observability.action_graph` event and emits a
stderr log line with:

- MCP client identity
- tool name
- status
- trace id

`harn.trigger.fire` also injects MCP client identity and trace metadata into the
synthetic event headers so downstream dispatch traces can be tied back to the
calling MCP client.

`harn.secret_scan` additionally appends `audit.secret_scan` records with only
redacted findings plus stable fingerprints so future trust-graph consumers can
reason about scan hygiene without storing raw secret material.
