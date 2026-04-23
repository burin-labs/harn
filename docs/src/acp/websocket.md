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

`session/load` can reload an active session in the current dispatcher. The
WebSocket transport also records connection and session lifecycle events on
`acp.session.<connection-id>` EventLog topics so durable replay can build on
the same audit trail.

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
