# Bridge Protocol

Harn's stdio bridge uses JSON-RPC 2.0 notifications and requests for host/runtime
coordination that sits below ACP session semantics.

## Tool lifecycle gates

Hosts can opt into request-response tool gates. These calls are best-effort: if the
host does not implement them, Harn proceeds normally.

### `tool/pre_use`

Sent as a bridge request before a tool call executes.

Request payload:

```json
{
  "tool_name": "list_directory",
  "tool_use_id": "call_123",
  "args": {"path": "."}
}
```

Response payload:

- `{ "action": "allow" }`: continue unchanged
- `{ "action": "deny", "reason": "..." }`: reject the tool call and surface the rejection in the transcript
- `{ "action": "modify", "args": {...} }`: replace arguments before execution

### `tool/post_use`

Sent as a bridge request after a tool call completes.

Request payload:

```json
{
  "tool_name": "list_directory",
  "tool_use_id": "call_123",
  "result": "...tool output...",
  "rejected": false
}
```

Response payload:

- `{ "result": "..." }`: replace the visible tool result text
- any other payload: leave the result unchanged

## Daemon idle/resume notifications

Daemon agents stay alive after text-only turns and wait for host activity with adaptive
backoff: `100ms`, `500ms`, `1s`, `2s`, resetting to `100ms` whenever activity arrives.

### `agent/idle`

Sent as a bridge notification whenever the daemon enters or remains in the idle wait loop.

Payload:

```json
{
  "iteration": 3,
  "backoff_ms": 1000
}
```

### `agent/resume`

Hosts can send this notification to wake an idle daemon without injecting a user-visible
message.

Payload:

```json
{}
```

A host may also wake the daemon by sending a queued `user_message`, `session/input`, or
`agent/user_message` notification.
