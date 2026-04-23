# ACP over WebSocket

`harn orchestrator serve` exposes ACP at:

```text
ws://<host>/acp
wss://<host>/acp
Authorization: Bearer <api-key>
```

Use `wss://` when the orchestrator is served with `--cert` and `--key`.
Plain `ws://` is intended for local development or trusted private networks.

## Authentication

If `HARN_ORCHESTRATOR_API_KEYS` is set, `/acp` requires
`Authorization: Bearer <api-key>` during the WebSocket upgrade. Failed
authentication returns `401 Unauthorized` before the upgrade completes.

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
- `session/cancel`
- `session/input`
- `session/fork`
- `agent/resume`
- `workflow/*`
- `harn.hitl.respond`

The transport assigns every outbound JSON-RPC frame a stable Harn extension
event id:

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
On reconnect, call `session/load` with that cursor:

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "session/load",
  "params": {
    "sessionId": "session-id",
    "lastAckedEventId": 42
  }
}
```

`session/load` attaches the new socket to the retained session worker when one
is still live. The server replays missed outbound frames with
`_harn.replayed = true`, then continues the same worker. This includes pending
JSON-RPC requests from Harn to the host, so a prompt that is waiting on
`host/capabilities`, `host/call`, or another host response can continue after
the host reconnects and responds to the replayed request.

Retained workers expire after 5 minutes by default. `HARN_ACP_WS_RETAIN_SECS`
can tune that window for controlled deployments and tests. After expiry, or
after an orchestrator process restart, Harn can still replay serialized outbound
frames from the EventLog topic `acp.session.<session-id>`, but it cannot resume
an in-flight VM stack that was waiting inside the expired process. In that case
the host should treat replay as recovery/audit state and start a new prompt.

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
