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
`AgentEvent::ToolCallUpdate` events; the packaged `harn-serve` ACP adapter
translates them into `session/update` notifications via an `AgentEventSink` it
registers per session.

### ACP Compatibility Contract

Harn tracks the upstream Agent Client Protocol schema and pins its
wire contract against `agentclientprotocol/agent-client-protocol` schema
`v0.12.2`. The adapter treats these `session/update` values as standard
ACP variants:

- `agent_message_chunk`
- `agent_thought_chunk`
- `tool_call`
- `tool_call_update`
- `plan`

Harn also emits host-facing lifecycle updates that are not ACP-standard.
They are intentionally kept as top-level `sessionUpdate` discriminators
for compatibility with existing Burin Code and other host renderers, and
are advertised during `initialize` under
`agentCapabilities._meta.harn.sessionUpdateExtensions`:

- `fs_watch`
- `handoff`
- `log`
- `progress`
- `skill_activated`
- `skill_deactivated`
- `skill_scope_tools`
- `tool_search_query`
- `tool_search_result`
- `transcript_compacted`
- `worker_update`

Hosts that do not recognize one of these values should ignore it using
normal ACP forward-compatibility behavior. Hosts that render Harn
extensions should key off the explicit extension list from `initialize`
instead of discovering behavior from a local allow-list.

Harn keeps several high-value tool-rendering fields at the top level of
`tool_call` / `tool_call_update` rather than moving them under `_meta`,
because downstream UIs already consume them directly. These field
extensions are also advertised during `initialize` under
`agentCapabilities._meta.harn.toolLifecycleExtensionFields`:

- `audit`
- `durationMs`
- `error`
- `errorCategory`
- `executionDurationMs`
- `executor`
- `parsing`
- `rawInputPartial`

The standard ACP fields (`toolCallId`, `title`, `kind`, `status`,
`content`, `locations`, `rawInput`, and `rawOutput`) remain available in
their ACP locations. Harn's pinned fixtures under
`crates/harn-serve/tests/fixtures/acp/` pin both the standard and
extension shapes so host integrations can reference stable examples.

### `audit` tag

Both `tool_call` and `tool_call_update` carry an optional `audit`
field that mirrors the active mutation session for the dispatch (see
[Trust boundary](../../spec/opentrustgraph.md)). Hosts use it to:

- group every tool emission belonging to the same write-capable
  session (so undo/redo and audit logs never cross sessions even when
  multiple workers run in parallel);
- correlate the canonical `tool_call` stream against
  `session/update.worker_update.audit` and the optional
  `session/request_permission.mutation` payloads — they all carry the
  same `MutationSessionRecord`, so a host that already understands one
  reuses the same codepath for the others;
- decide whether to surface a tool dispatch in trust-boundary UX
  (e.g. badge writes that escape `mutation_scope: read_only`) without
  guessing from the tool name.

Wire shape (snake_case fields, matching the existing `worker_update.audit`
contract):

```json
{
  "audit": {
    "session_id": "session_42",
    "parent_session_id": "session_root",
    "run_id": "run_42",
    "worker_id": "worker_3",
    "execution_kind": "worker",
    "mutation_scope": "apply_workspace",
    "approval_policy": {
      "auto_approve": [],
      "auto_deny": [],
      "require_approval": ["edit_*"],
      "write_path_allowlist": ["src/**"]
    }
  }
}
```

The field is omitted when no mutation session is installed (read-only
`harn run` invocations, conformance fixtures, scripts that don't enter
a workflow). Existing clients that don't know about `audit` see the
same wire shape they always did.

### `executor` tag

`tool_call_update` carries an optional `executor` field that names the
backend that ran the tool, so clients can render "via mcp:linear" /
"via host bridge" badges, attribute latency by transport, and route
errors correctly. Variants:

- `"harn_builtin"` — VM-stdlib (e.g. `read_file`, `write_file`,
  `exec`, `http_*`, `mcp_*`) or any Harn-side handler closure
  registered in the script's `tools` table.
- `"host_bridge"` — capability provided by the host through the bridge
  (Swift IDE bridge, BurinApp, BurinCLI host shells).
- `{"kind": "mcp_server", "serverName": "<name>"}` — tool came from
  `mcp_list_tools` against the named server. The agent loop detects
  this from the `_mcp_server` annotation `mcp_list_tools` injects on
  every dict, so the tag survives even when the call physically
  proxies through a bridge.
- `"provider_native"` — provider executed the tool server-side and
  inlined the result (currently only OpenAI Responses-API
  `tool_search` and the equivalent Anthropic native search; the agent
  never dispatches these locally).

Unit variants serialize as bare strings; the `mcp_server` case carries
the configured server name. The field is omitted when unknown — most
commonly for the in-progress emission that fires before the dispatch
backend is picked.

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

### Lifecycle states

The runtime emits one of six typed lifecycle events per worker
transition. The wire `status` string is the same value harn writes to
the worker's persisted state, so it round-trips through the bridge
unchanged:

| Event                   | `status`         | Meaning                                                                                                            |
| ----------------------- | ---------------- | ------------------------------------------------------------------------------------------------------------------ |
| `WorkerSpawned`         | `running`        | A worker (delegated stage, workflow, or sub-agent) has begun a new cycle.                                          |
| `WorkerProgressed`      | `progressed`     | A retriggerable worker is resuming after `worker_trigger`. Followed shortly by another `running` from the new cycle. |
| `WorkerWaitingForInput` | `awaiting_input` | A retriggerable worker has finished its current cycle and is parked waiting for the next host trigger payload.     |
| `WorkerCompleted`       | `completed`      | A non-retriggerable worker has finished successfully (terminal).                                                   |
| `WorkerFailed`          | `failed`         | A worker terminated with an error (terminal).                                                                      |
| `WorkerCancelled`       | `cancelled`      | A worker was cancelled via `close_agent` or upstream cancellation (terminal).                                      |

The three terminal events end the worker's lifetime. `Progressed` and
`WaitingForInput` are explicitly non-terminal — observers should
expect more events on the same `worker_id` after they fire.

### `worker_update` notification shape

ACP and A2A adapters subscribe to the canonical `AgentEvent::WorkerUpdate`
variant and translate it into their respective wire formats from one
typed source. ACP emits a `session/update` with `sessionUpdate:
"worker_update"`:

```json
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {
    "sessionId": "session_123",
    "update": {
      "sessionUpdate": "worker_update",
      "workerId": "worker_abc",
      "workerName": "review_captain",
      "workerTask": "Review PR #42",
      "workerMode": "delegated_stage",
      "event": "WorkerWaitingForInput",
      "status": "awaiting_input",
      "terminal": false,
      "metadata": {
        "task": "Review PR #42",
        "mode": "delegated_stage",
        "started_at": "0193...",
        "finished_at": null,
        "awaiting_started_at": "0193...",
        "child_run_id": "run_xyz",
        "child_run_path": ".harn-runs/run_xyz",
        "snapshot_path": ".harn/workers/worker_abc.json",
        "audit": { "...": "MutationSessionRecord" },
        "error": null
      },
      "audit": { "...": "MutationSessionRecord" }
    }
  }
}
```

The `event` discriminator is the typed `WorkerEvent` variant name; the
`status` field is the same lower-case value the legacy bridge `status`
field carried. `terminal` is a derived hint so clients can decide whether
to retain the worker in their tracking UI without parsing the event
name.

A2A surfaces the same event as a task-stream entry of type
`worker_update`, scoped to the task whose dispatch spawned the worker.
The payload mirrors the ACP shape (worker fields under camelCase keys
plus `metadata`/`audit`). Subscribers receive these alongside the
existing `status`/`message` events on the SSE / push-notification
streams.

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

## Host tool discovery

Hosts can expose their own dynamic tool surface to scripts without
pre-registering every tool in the initial prompt. Harn discovers that
surface through one bridge RPC and then invokes individual tools
through the existing `builtin_call` request path.

### `host/tools/list`

VM-issued request. No parameters (or an empty object). The host
responds with a list of tool descriptors. Canonical response shape:

```json
{
  "tools": [
    {
      "name": "Read",
      "description": "Read a file from the active workspace",
      "schema": {
        "type": "object",
        "properties": {
          "path": {"type": "string", "description": "File path to read"}
        },
        "required": ["path"]
      },
      "deprecated": false
    },
    {
      "name": "open_file",
      "description": "Reveal a file in the editor",
      "schema": {
        "type": "object",
        "properties": {
          "path": {"type": "string"}
        },
        "required": ["path"]
      },
      "deprecated": true
    }
  ]
}
```

Accepted variants:

- a bare array `[{...}, {...}]`
- an ACP-style wrapper `{ "result": { "tools": [...] } }`
- compatibility field names `short_description`, `parameters`, or
  `input_schema`; Harn normalizes them to `description` and `schema`

Each normalized descriptor surfaced to scripts has exactly these keys:

- `name`: string, required
- `description`: string, defaults to `""`
- `schema`: JSON Schema object or `null`
- `deprecated`: boolean, defaults to `false`

Invocation:

- `host_tool_list()` returns the normalized list directly.
- `host_tool_call(name, args)` then dispatches that tool through the
  existing `builtin_call` bridge request using `name` as the builtin
  name and `args` as the single argument payload.

## Skill registry (issue #73)

Hosts expose their own managed skill store to the VM through three RPCs.
Filesystem skill discovery works without the bridge (`harn run` walks
the seven non-host layers described in [Skills](./skills.md)); these
RPCs add a layer 8 so cloud hosts, enterprise deployments, and IDE
hosts can serve skills the filesystem can't see.

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

## Host-delegated skill matching

Harn agents that opt into
`skill_match: { strategy: "host" }` (or the alias `"embedding"`)
delegate skill ranking to the host via a single JSON-RPC request. The
host response is purely advisory — unknown skill names are ignored,
and an RPC error falls back to the in-VM metadata ranker with a
warning logged against `agent.skill_match`.

### `skill/match`

Request payload (harn-issued, host response required):

```json
{
  "strategy": "host",
  "prompt": "Ship the new release to production",
  "working_files": ["infra/terraform/cluster.tf"],
  "candidates": [
    {
      "name": "ship",
      "description": "Ship a production release",
      "when_to_use": "User says ship/release/deploy",
      "paths": ["infra/**", "Dockerfile"]
    },
    {
      "name": "review",
      "description": "Review existing code for correctness",
      "when_to_use": "User asks to review/audit",
      "paths": []
    }
  ]
}
```

Response payload (host-issued):

```json
{
  "matches": [
    {"name": "ship", "score": 0.92, "reason": "matched by embedding similarity"}
  ]
}
```

- `matches[*].name` (required): the candidate's skill name. Names
  absent from the original `candidates` list are ignored.
- `matches[*].score` (optional): non-negative float; higher scores
  rank earlier. Defaults to `1.0` when omitted.
- `matches[*].reason` (optional): short diagnostic stored on the
  `skill_matched` / `skill_activated` transcript events. Defaults
  to `"host match"`.

Alternative shapes accepted for host convenience:

- Top-level array: `[{"name": ..., "score": ...}, ...]`
- `{"skills": [...]}` wrapping
- `{"result": {"matches": [...]}}` ACP envelope

### Skill lifecycle session updates

Agents emit ACP `session/update` notifications for skill lifecycle
transitions so hosts can surface active-skill state in real time.
These are Harn extension variants advertised during `initialize`, not
upstream ACP `SessionUpdate` variants.
The packaged `harn-serve` ACP adapter translates the canonical `AgentEvent`
variants into:

- `sessionUpdate: "skill_activated"` — `{skillName, iteration, reason}`
- `sessionUpdate: "skill_deactivated"` — `{skillName, iteration}`
- `sessionUpdate: "skill_scope_tools"` — `{skillName, allowedTools}`

`skill_matched` stays internal to the VM transcript — the candidate
list can be large and host UIs typically only care about activation
transitions, not every ranking pass.
