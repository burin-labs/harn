# ACP over WebSocket

Harn exposes ACP over WebSocket in two places:

- `harn serve acp --transport websocket <file.harn>` starts a direct
  single-socket ACP endpoint for editor hosts that want to attach to one Harn
  agent server.
- `harn orchestrator serve` mounts the retained multi-client hub at `/acp` for
  browser or remote IDE integrations that need attach/replay semantics.

The direct endpoint defaults to:

```text
ws://127.0.0.1:8789/acp
```

The orchestrator endpoint is:

```text
ws://<host>/acp
wss://<host>/acp
Authorization: Bearer <api-key>
```

This page covers only the WebSocket transport. The canonical ACP behavior guide
is [MCP, ACP, and A2A integration](../mcp-and-acp.md#acp-agent-client-protocol);
the broader entry-point table is [Protocol support matrix](../protocol-support.md).

Use `wss://` when the server is served with `--cert` and `--key`. Plain
`ws://` is intended for local development or trusted private networks.

## Authentication

For `harn serve acp --transport websocket`, `--api-key` /
`HARN_SERVE_API_KEY` advertises ACP `authMethods`. Clients may either send
`Authorization: Bearer <api-key>` or `X-API-Key` during the WebSocket upgrade
to pre-authorize the connection, or connect without those headers and complete
the normal in-band ACP `authenticate` method after `initialize`.

For `harn orchestrator serve`, if `HARN_ORCHESTRATOR_API_KEYS` is set, `/acp`
requires `Authorization: Bearer <api-key>` during the WebSocket upgrade.
Failed authentication returns `401 Unauthorized` before the upgrade completes.

The endpoint uses the orchestrator listener origin guard. Configure browser
origins in `harn.toml`:

```toml
[orchestrator]
allowed_origins = ["https://ide.example.com"]
```

## Framing

ACP messages are JSON-RPC 2.0 objects sent as individual WebSocket text frames.
There is no NDJSON, SSE, or extra wrapper envelope.

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
```

The server responds with one JSON-RPC object in one text frame. Binary frames
are rejected with JSON-RPC `Invalid Request`.

## Harn-native ingress

Harnesses that need to host ACP-style browser or agent streams can expose an
upgrade route directly from Harn:

```harn
pipeline acp_websocket_echo() {
  let server = websocket_server("127.0.0.1:8787", {})
  websocket_route(server, "/acp", {
    auth: {bearer: env("ACP_TOKEN")},
    max_message_bytes: 1048576,
    send_buffer_messages: 64,
    idle_timeout_ms: 30000,
  })

  while true {
    let accepted = websocket_accept(server, 30000)
    if accepted == nil || accepted?.type == "timeout" {
      continue
    }
    let conn = accepted ?? {}

    let frame = websocket_receive(conn, 30000) ?? {}
    if frame?.type == "text" {
      let request = json_parse(frame.data)
      websocket_send(conn, json_stringify({
        jsonrpc: "2.0",
        id: request.id,
        result: {echo: request.method},
      }), {})
    } else if frame?.type == "close" {
      websocket_close(conn)
    }
  }
}
```

## Liveness

The server sends WebSocket ping frames every 30 seconds. If a pong is not
observed within 10 seconds, the server closes the connection and records a
liveness timeout event.

## Sessions

Each WebSocket connection runs an ACP dispatcher backed by the same method
surface as stdio ACP:

- `initialize`
- `session/new`
- `session/load`
- `session/list`
- `session/prompt`
- `session/inject`
- `session/revoke_inject`
- `session/replace_inject`
- `session/cancel`
- `session/truncate`
- `session/rollback`
- `session/redo`
- `session/close`
- `session/remind`
- `session/pending_injections`
- `session/revoke_reminder`
- `session/fork`
- `session/set_mode`
- `session/set_config_option`
- `harn.session_workspace_roots`
- `harn.session_add_root`
- `harn.session_reanchor`
- `agent/resume`
- `workflow/*`
- `harn.hitl.respond`

The direct `harn serve acp --transport websocket` endpoint runs one ACP server
per socket. Use the orchestrator `/acp` hub when clients need retained sessions,
multi-client attach, role arbitration, or replay.

## Retained Orchestrator Sessions

The orchestrator transport assigns every outbound JSON-RPC frame a stable Harn
extension event id:

```json
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {},
  "_harn": {
    "eventId": 42,
    "sessionId": "session-id",
    "replayed": false
  }
}
```

Clients should persist the highest `_harn.eventId` they have durably processed.
Before reconnecting a specific session, clients can discover attachable sessions
with `session/list`. Harn accepts `workspaceAnchor` as the preferred project key
and `cwd` as a compatibility fallback:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "session/list",
  "params": {
    "workspaceAnchor": { "primary": "/workspace/harn" },
    "liveState": ["live", "detached_retained"]
  }
}
```

Each result includes `sessionId`, `cwd` when known, `workspaceAnchor` when the
session has one, `liveState`, `attachableRoles`, and `lastEventId` when replay
metadata is available. `liveState` is one of:

- `live`: a retained worker is still running.
- `detached_retained`: a retained worker is waiting for a host owner reconnect.
- `expired_replay_only`: only serialized audit/replay frames remain.

`attachableRoles` uses Harn's host-owner model:

- `host_owner` is the single authoritative client that receives host JSON-RPC
  requests and may answer them.
- `observer` clients receive replay, live `session/update` frames, and
  `_harn/presence` notifications, but cannot send session controls.
- `controller` clients may send bounded session controls such as
  `session/cancel`, `session/inject`, `session/revoke_inject`,
  `session/replace_inject`, `session/truncate`, `session/remind`,
  `session/pending_injections`, `session/revoke_reminder`, `session/set_mode`,
  and `session/set_config_option`. General host requests
  are still delivered only to the `host_owner`; `session/request_permission`
  is also delivered to controllers so an attached control surface can answer
  approval prompts.

Legacy clients that omit a role request `host_owner`, preserving the old
single-host behavior. To attach a read-only client, pass the role through
Harn's extension metadata:

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "session/load",
  "params": {
    "sessionId": "session-id",
    "lastAckedEventId": 42,
    "_harn": {
      "role": "observer",
      "clientId": "ide-panel-1"
    }
  }
}
```

`session/load` attaches the new socket to the retained session worker when one
is still live. For non-owner roles, the hub returns the session metadata and
role capabilities directly. The server replays missed outbound frames with
`_harn.replayed = true`, then continues the same worker. Host-owner replay
includes pending JSON-RPC requests from Harn to the host, so a prompt that is
waiting on `host/capabilities`, `host/call`, or another host response can
continue after the host reconnects and responds to the replayed request.
Observer/controller replay excludes host requests and response ids that belong
to other clients; those clients receive broadcast notifications and presence.

Attach and detach changes are emitted as JSON-RPC notifications:

```json
{
  "jsonrpc": "2.0",
  "method": "_harn/presence",
  "params": {
    "sessionId": "session-id",
    "clientId": "ide-panel-1",
    "connectionId": "connection-id",
    "role": "observer",
    "state": "attached"
  },
  "_harn": {
    "eventId": 43,
    "sessionId": "session-id",
    "replayed": false
  }
}
```

Read-only control attempts fail with a structured JSON-RPC error:

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "error": {
    "code": -32011,
    "message": "ACP client role is not authorized for this method",
    "data": {
      "method": "session/prompt",
      "role": "observer",
      "reason": "role_not_authorized"
    }
  }
}
```

Permission decisions are deterministic when multiple control-capable clients
race on the same `session/request_permission` request:

- the first valid response wins and is forwarded to the running ACP worker;
- the same response from the same actor/request id is idempotent and returns
  `status: "already_applied"`;
- conflicting late responses fail with `code: -32013` and `data.reason:
  "already_decided"`, including `decidedBy`, `decidedAtMs`, and the attempted
  actor.

Forwarded controls carry `_harn.actor` with `clientId`, `connectionId`, `role`,
and `source: "websocket"`. ACP adapter audit/replay emits matching
`_harn/agentEvent` frames with `kind: "control_outcome"` for accepted,
idempotent, and rejected outcomes.

Retained workers expire after 5 minutes by default. `HARN_ACP_WS_RETAIN_SECS`
can tune that window for controlled deployments and tests. After expiry, or
after an orchestrator process restart, Harn can still replay serialized outbound
frames from the EventLog topic `acp.session.<session-id>`, but it cannot resume
an in-flight VM stack that was waiting inside the expired process. In that case
`session/load` returns `liveState: "expired_replay_only"` and the host should
treat replay as recovery/audit state before starting a new prompt.

## Example

Node clients can set the `Authorization` header directly. Browser-hosted
clients usually need a trusted backend or extension host to perform the
authenticated upgrade because the browser `WebSocket` API does not allow
custom headers.

```js
import WebSocket from "ws";

const socket = new WebSocket("wss://orchestrator.example.com/acp", {
  headers: { Authorization: "Bearer " + apiKey },
});

socket.addEventListener("open", () => {
  socket.send(JSON.stringify({
    jsonrpc: "2.0",
    id: 1,
    method: "initialize",
    params: {},
  }));
});

socket.addEventListener("message", (event) => {
  const message = JSON.parse(event.data);
  console.log(message);
});
```
