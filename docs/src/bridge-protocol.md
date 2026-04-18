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

## Client-executed tool search

When a Harn script opts into `tool_search` against a provider that lacks
native defer-loading support, the runtime switches to a client-executed
fallback (see the [LLM and agents guide](./llm-and-agents.md)). For the
`"bm25"` and `"regex"` strategies everything stays in-VM; the
`"semantic"` and `"host"` strategies round-trip the query through the
bridge.

### `tool_search/query`

Request payload (harn-issued, host response required):

```json
{
  "strategy": "semantic",
  "query": "deploy a new service version",
  "candidates": ["deploy_service", "rollback_service", "query_metrics", "..."]
}
```

- `strategy`: one of `"semantic"` or `"host"`. The in-tree strategies
  (`"bm25"` / `"regex"`) never hit the bridge.
- `query`: the raw query string the model passed to the synthetic
  search tool. For `strategy: "regex"` / `"bm25"` hosts *don't* see
  this; those strategies run inside the VM.
- `candidates`: full list of deferred tool names the host may choose
  from. The host should return a subset.

Response payload (host-issued):

```json
{
  "tool_names": ["deploy_service", "rollback_service"],
  "diagnostic": "matched by vector similarity"
}
```

- `tool_names` (required): ordered list of tool names to promote.
  Unknown names are ignored by the runtime — they can't be surfaced
  because their schemas weren't registered. Return at most ~20 names
  per call; the runtime caps promotions soft-per-turn regardless.
- `diagnostic` (optional): short explanation surfaced to the model in
  the tool result alongside `tool_names`. Useful for "no hits, try
  broader terms"-style feedback.

An ACP-style wrapper `{ "result": { "tool_names": [...] } }` is also
accepted for hosts that re-wrap everything in a `result` envelope.

Errors: a JSON-RPC error response (standard shape) is surfaced to the
model as a `tool_names: []` result with a diagnostic that includes the
host error message. The loop continues — the model can retry with a
different query.

## Skill registry (issue #73)

Hosts expose their own managed skill store to the VM through three RPCs.
Filesystem skill discovery works without the bridge (`harn run` walks
the seven non-host layers described in [Skills](./skills.md)); these
RPCs add a layer 8 so cloud hosts, enterprise deployments, and the
Burin Code IDE can serve skills the filesystem can't see.

### `skills/list`

VM-issued request. No parameters (or an empty object). The host
responds with an array of `SkillManifestRef` entries. Minimal shape:

```json
[
  { "id": "deploy", "name": "deploy", "description": "Ship it", "source": "host" },
  { "id": "acme/ops/review", "name": "review", "description": "Code review", "source": "host" }
]
```

The VM also accepts `{ "skills": [ ... ] }` for hosts that wrap
collections in an object.

### `skills/fetch`

VM-issued request. Parameters: `{ "id": "<skill id>" }`. Response is a
single skill object carrying enough metadata to populate a `Skill`:

```json
{
  "name": "deploy",
  "description": "Ship it",
  "body": "# Deploy runbook\n...",
  "manifest": {
    "when_to_use": "...",
    "allowed_tools": ["bash", "git"],
    "paths": ["infra/**"],
    "model": "claude-opus-4-7"
  }
}
```

Hosts may flatten the manifest fields into the top level instead — the
CLI accepts either shape.

### `skills/update`

Host-issued notification. No parameters. Invalidates the VM's cached
skill catalog; the CLI re-runs layered discovery (including another
`skills/list` call) on the next iteration boundary — for `harn watch`,
between file changes; for long-running agents, between turns. A VM
without an active bridge simply ignores the notification.
