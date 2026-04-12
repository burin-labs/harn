# Bridge protocol

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
  "args": {"path": "."},
  "mutation": {
    "classification": "read_only",
    "declared_paths": ["."],
    "session": {
      "session_id": "session_123",
      "run_id": "run_123",
      "worker_id": null,
      "mutation_scope": "read_only",
      "approval_policy": null
    }
  }
}
```

Response payload:

- `{ "action": "allow" }`: continue unchanged
- `{ "action": "deny", "reason": "..." }`: reject the tool call and surface the rejection in the transcript
- `{ "action": "modify", "args": {...} }`: replace arguments before execution

`mutation.classification` is advisory runtime metadata from Harn. Hosts may
apply stricter local policy.

### `tool/request_approval`

Sent as a bridge request when the active `ToolApprovalPolicy` classifies the
call as `RequiresHostApproval` (matching a `require_approval` pattern). The
host is expected to prompt the user and return a decision. Unlike
`tool/pre_use`, this call **fails closed**: if the host does not implement it
or returns an error, the tool is denied.

Request payload:

```json
{
  "tool_name": "edit_file",
  "tool_use_id": "call_123",
  "args": {"path": "src/main.rs"},
  "declared_paths": ["src/main.rs"],
  "mutation": {
    "session_id": "session_123",
    "run_id": "run_123",
    "worker_id": null,
    "mutation_scope": "apply_workspace",
    "approval_policy": {"require_approval": ["edit*"]}
  }
}
```

Response payload:

- `{ "granted": true }`: proceed with the original arguments
- `{ "granted": true, "args": {...} }`: proceed with rewritten arguments
- `{ "granted": false, "reason": "..." }`: reject the tool call

Use `tool/pre_use` for passive host-side filtering that does not need user
input (audit logging, automatic allow/deny from stored rules); use
`tool/request_approval` for any call that should surface an interactive prompt.

### `tool/post_use`

Sent as a bridge request after a tool call completes.

Request payload:

```json
{
  "tool_name": "list_directory",
  "tool_use_id": "call_123",
  "result": "...tool output...",
  "rejected": false,
  "mutation": {
    "classification": "read_only",
    "declared_paths": ["."],
    "session": {
      "session_id": "session_123",
      "run_id": "run_123",
      "worker_id": null,
      "mutation_scope": "read_only",
      "approval_policy": null
    }
  }
}
```

Response payload:

- `{ "result": "..." }`: replace the visible tool result text
- any other payload: leave the result unchanged

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
