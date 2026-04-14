# Bridge protocol

Harn's stdio bridge uses JSON-RPC 2.0 notifications and requests for host/runtime
coordination that sits below ACP session semantics.

## Tool lifecycle observation

The `tool/pre_use`, `tool/post_use`, and `tool/request_approval` bridge
request/response methods have been **retired** in favor of the canonical
ACP surface:

- Tool lifecycle is now carried on the `session/update` notification
  stream as `tool_call` and `tool_call_update` variants (see the ACP
  schema at <https://agentclientprotocol.com/protocol/schema>). Hosts
  observe every dispatch via the session update stream — there is no
  host-side approve/deny/modify hook at dispatch time.
- Approvals route through canonical `session/request_permission`. When
  harn's declarative `ToolApprovalPolicy` classifies a call as
  `RequiresHostApproval`, the agent loop issues a
  `session/request_permission` request to the host and **fails closed**
  if the host does not implement it (or returns an error).

Internally, the agent loop emits `AgentEvent::ToolCall` +
`AgentEvent::ToolCallUpdate` events; `harn-cli`'s ACP server translates
them into `session/update` notifications via an `AgentEventSink` it
registers per session.

### `session/request_permission`

Request payload (harn-issued):

```json
{
  "sessionId": "session_123",
  "toolCall": {
    "toolCallId": "call_123",
    "toolName": "edit_file",
    "rawInput": {"path": "src/main.rs"}
  },
  "mutation": {
    "session_id": "session_123",
    "run_id": "run_123",
    "worker_id": null,
    "mutation_scope": "apply_workspace",
    "approval_policy": {"require_approval": ["edit*"]}
  },
  "declaredPaths": ["src/main.rs"]
}
```

Response payload (host-issued):

- `{ "outcome": { "outcome": "selected" } }` (ACP canonical): granted
- `{ "granted": true }` (legacy shim): granted with original args
- `{ "granted": true, "args": {...} }`: granted with rewritten args
- `{ "granted": false, "reason": "..." }`: denied

## Worker lifecycle notifications

Delegated workers emit `session/update` notifications with `worker_update`
content. Those payloads include lifecycle timing, child run/snapshot paths,
and audit-session metadata so hosts can render background work without
scraping plain-text logs.

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
