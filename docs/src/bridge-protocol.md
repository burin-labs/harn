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

### ACP compatibility contract

Harn-owned ACP/MCP extension fields are specified in
[Harn ACP/MCP extensions v1](./spec/harn-extensions/v1.md). Harn tracks the
upstream Agent Client Protocol schema and pins its
wire contract against `agentclientprotocol/agent-client-protocol` schema
`v0.12.2`. The adapter treats these `session/update` values as standard
ACP variants:

- `user_message`
- `user_message_chunk`
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

- `available_commands_update`
- `fs_watch`
- `handoff`
- `hitl_request`
- `hitl_resolved`
- `log`
- `progress`
- `reminder_emitted`
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

Harn keeps its tool-rendering extensions under `tool_call._meta.harn` and
`tool_call_update._meta.harn`, following ACP's extension convention while
leaving canonical ACP fields at their standard locations. These Harn
metadata keys are advertised during `initialize` under
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
their ACP locations. Hosts migrating from pre-#904 builds should read the
listed Harn fields from `_meta.harn` instead of the tool-call update root.
Harn's pinned fixtures under `crates/harn-serve/tests/fixtures/acp/` pin
both the standard and extension shapes so host integrations can reference
stable examples.

#### Vendor-extension session-update payload namespacing (#905)

Following the
[ACP extensibility convention](https://agentclientprotocol.com/protocol/extensibility),
**every vendor field on a Harn extension session-update is emitted under
`update._meta.harn`** — never as a bare top-level field. The `sessionUpdate`
discriminator stays at the canonical location so existing dispatchers
that route on it keep working unchanged. Concretely:

| Update variant         | Vendor fields under `_meta.harn`                                                                                                       |
| ---------------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| `progress`             | `phase`, `message`, `progress`, `total`, `data`                                                                                        |
| `log`                  | `level`, `message`, `fields`                                                                                                           |
| `fs_watch`             | `subscriptionId`, `events`                                                                                                             |
| `worker_update`        | `workerId`, `workerName`, `workerTask`, `workerMode`, `event`, `status`, `terminal`, `metadata`, `audit`                               |
| `transcript_compacted` | `mode`, `strategy`, `archivedMessages`, `estimatedTokensBefore`, `estimatedTokensAfter`, `snapshotAssetId`, `instructionMode`, `instructionSource`, `compactionPolicy` |
| `handoff`              | `handoffId`, `artifactId`, `handoff`                                                                                                   |
| `skill_activated`      | `skillName`, `iteration`, `reason`                                                                                                     |
| `skill_deactivated`    | `skillName`, `iteration`                                                                                                               |
| `skill_scope_tools`    | `skillName`, `allowedTools`                                                                                                            |
| `tool_search_query`    | `toolUseId`, `name`, `query`, `strategy`, `mode`                                                                                       |
| `tool_search_result`   | `toolUseId`, `promoted`, `strategy`, `mode`                                                                                            |
| `hitl_request`         | `requestId`, `kind`, `payload`                                                                                                         |
| `hitl_resolved`        | `requestId`, `kind`, `outcome`                                                                                                         |
| `reminder_emitted`     | `reminder`                                                                                                                             |

Hosts migrating from pre-#905 builds must read these fields from
`_meta.harn.<field>` instead of the update root. The fixture
`crates/harn-serve/tests/fixtures/acp/session_update_extensions.json`
pins the new wire shape verbatim.

`reminder_emitted` is sent when a pending system reminder is rendered
into the next model request. Its payload lives at
`update._meta.harn.reminder`:

```json
{
  "sessionUpdate": "reminder_emitted",
  "_meta": {
    "harn": {
      "reminder": {
        "reminderId": "reminder-1",
        "tags": ["token_pressure"],
        "body": "Refresh the compacted context before answering.",
        "roleHint": "developer",
        "renderedRole": "developer",
        "source": "stdlib_provider",
        "ttlTurns": 2
      }
    }
  }
}
```

Content extensions (`visible_text` and `visible_delta` on the
`agent_message_chunk` content block, advertised via
`agentCapabilities._meta.harn.contentExtensionFields`) follow the same
convention but ride under `content._meta.harn` because they extend the
canonical ACP content block, not the session-update envelope. Example:

```json
{
  "sessionUpdate": "agent_message_chunk",
  "content": {
    "type": "text",
    "text": "hello",
    "_meta": {
      "harn": {
        "visible_text": "hello",
        "visible_delta": "hello"
      }
    }
  }
}
```

### Composition event migration note

Governed Code Mode composition runs are represented on the Harn
`_harn/agentEvent` extension channel, not as one opaque tool call and
not as new ACP `session/update` variants. Consumers that do not render
composition events can ignore unknown `_harn/agentEvent.kind` values and
continue reading ordinary `tool_call` / `tool_call_update` events.

The Harn-native MVP is surfaced by `composition_binding_manifest(...)`,
`composition_execute(...)`, and the `std/composition`
`composition_mcp_tools(...)` profile. Binding manifests are hashable prompt
contracts; execution reports carry the same parent/child fields described here.
TypeScript declarations generated from a manifest are editor/model affordances,
not the bridge authority.

Consumers that do render them should group by `runId`:

- `composition_start` identifies the snippet language, snippet hash,
  binding-manifest hash, and requested side-effect ceiling.
- `composition_child_call` records each child binding operation with
  `rawInput`, `policyContext`, `requestedSideEffectLevel`, and the
  `ToolAnnotations` used for policy decisions.
- `composition_child_result` records the child status, executor,
  output/error, and timing.
- `composition_finish` and `composition_error` are terminal parent
  events carrying stdout, stderr, artifacts, structured result, duration,
  and failure category.

Portal and transcript indexers should treat composition events as a
parent/child grouping overlay. A mutating child remains a mutating child
operation because its own annotations and side-effect level are present;
do not infer the whole run is read-only from the parent event alone.

### `audit` tag

Both `tool_call` and `tool_call_update` carry an optional `_meta.harn.audit`
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
  "_meta": {
    "harn": {
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
  }
}
```

The `_meta.harn.audit` field is omitted when no mutation session is installed (read-only
`harn run` invocations, conformance fixtures, scripts that don't enter
a workflow). Worker lifecycle audit data is carried as
`worker_update._meta.harn.audit`, matching the Harn extension namespacing
contract.

### `executor` tag

`tool_call_update` carries an optional `_meta.harn.executor` field that names the
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
  "approvalRequest": {
    "id": "tool-call_123",
    "action": "edit_file",
    "args": {"path": "src/main.rs"},
    "principal": "worker_3",
    "requested_at": "2026-04-30T12:00:00Z",
    "approvers_required": 1,
    "evidence_refs": [
      {
        "kind": "workspace_path",
        "ref": "src/main.rs",
        "metadata": {"workspace_path": "src/main.rs"}
      }
    ],
    "undo_metadata": {
      "session_id": "session_123",
      "run_id": "run_123",
      "worker_id": null,
      "mutation_scope": "apply_workspace",
      "policy_decision": {
        "type": "harn.permission_policy_decision.v1",
        "action": "ask",
        "reason": "workspace edit",
        "matched_rule": {"source": "rules", "action": "ask", "id": "edit-src", "index": 0},
        "risk_labels": ["approval_required", "path_rule"]
      }
    },
    "capabilities_requested": ["workspace.write_text"]
  },
  "policyDecision": {
    "type": "harn.permission_policy_decision.v1",
    "action": "ask",
    "reason": "workspace edit",
    "matched_rule": {"source": "rules", "action": "ask", "id": "edit-src", "index": 0},
    "risk_labels": ["approval_required", "path_rule"],
    "context": {
      "tool_name": "edit_file",
      "tool_kind": "edit",
      "side_effect": "workspace_write",
      "paths": [{"workspace_path": "src/main.rs"}]
    }
  },
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

`approvalRequest` is the canonical Harn `ApprovalRequest` payload. Hosts should
render approval UI from that object and use `policyDecision` as the policy
rationale: it includes the matched rule, risk labels, normalized context, and
the action (`ask` for host approval requests). The same receipt is copied into
`approvalRequest.undo_metadata.policy_decision` and into
`PermissionGrant`/`PermissionDeny` transcript-event metadata so approvals are
auditable and replayable even when the host only persists the canonical request.
Treat the surrounding `toolCall`, `mutation`, and `declaredPaths` fields as
compatibility/context fields. The same shape is used in stdlib HITL notifications at
`harn.hitl.requested.params.payload.approval_request`, so Burin Code,
harn-cloud approval inboxes, and CLI-style hosts can share one renderer.

Field meanings:

- `id`: stable approval request id. For tool permission prompts this is the
  emitted tool-call id; for stdlib HITL it is the HITL request id.
- `action`: human-readable action name, usually the tool or stdlib action.
- `args`: structured action arguments safe for host rendering.
- `principal`: agent, worker, session, or other actor requesting approval.
- `requested_at`: RFC3339 UTC timestamp.
- `deadline`: optional RFC3339 UTC deadline. Omitted when there is no deadline.
- `approvers_required`: number of approving reviewers required to proceed.
- `evidence_refs`: host-renderable evidence handles, such as declared workspace
  paths or run artifacts.
- `undo_metadata`: mutation/audit metadata a host can use to group undo/redo.
- `capabilities_requested`: canonical capability operation names requested by
  the action.

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
"worker_update"`. Per the [vendor-extension namespacing
rule](#vendor-extension-session-update-payload-namespacing-905), all
worker fields ride under `update._meta.harn`:

```json
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {
    "sessionId": "session_123",
    "update": {
      "sessionUpdate": "worker_update",
      "_meta": {
        "harn": {
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
A2A is a separate protocol surface and keeps the worker fields at the
task-stream entry root (it has its own envelope conventions). ACP hosts
should always read worker fields from `update._meta.harn.<field>`.

Structured Harn plan emissions use the same task stream. When an agent calls
`emit_plan` or `update_plan`, A2A subscribers receive a `harn_plan` entry with
standard ACP-compatible `entries` plus the normalized `harn.plan.v1` artifact
under `plan`.

Agent progress reports use protocol-native A2A status updates. When
`agent_progress` emits narration or entries, task-stream subscribers receive a
non-terminal `status-update` event with `type: "status"`,
`status.state: "working"`, a populated `status.message`, and `final: false`.
Entry lists render as a deterministic markdown checklist inside the message
text. Final task completion still emits the terminal `completed` status
separately.

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

On the ACP adapter surface, hosts can wake the daemon by sending a pending
user-message inject. `session/inject` accepts `{sessionId, mode, content}`
where `content` is a string or ACP content-block array, then responds
immediately with an agent-owned `messageId`. Harn later delivers the same id
as `session/update` with `sessionUpdate: "user_message"`. `mode: "steer"` maps
to the next safe operation boundary, and `mode: "queue"` maps to
end-of-interaction delivery. Pending injects can be revoked with
`session/revoke_inject` or edited in place with `session/replace_inject` before
delivery.

To inject ambient context without pretending it is a user message, send
`session/remind`:

```json
{
  "body": "The workspace changed while you were idle; re-read src/lib.rs before editing.",
  "tags": ["workspace"],
  "dedupe_key": "workspace-change",
  "ttl_turns": 2,
  "role_hint": "system",
  "mode": "interrupt_immediate",
  "_meta": {
    "harn": {
      "origin": "file-watcher"
    }
  }
}
```

`mode` accepts the same delivery values as queued user messages:
`interrupt_immediate`, `finish_step`, and `wait_for_completion`. The reminder
payload is validated as a reminder spec; non-standard host metadata belongs under
`_meta`. Malformed reminder payloads are rejected with `HARN-RMD-002`; unknown
top-level reminder options are rejected with `HARN-RMD-001`.

## Client-executed tool search

When a Harn script opts into `tool_search` against a provider that lacks
native defer-loading support, the runtime switches to a client-executed
fallback (see the [LLM tools guide](./llm/tools.md#client-executed-fallback)).
Built-in `bm25`, `regex`, and `hybrid` strategies stay in Harn/VM space. Hosts
that want embeddings, LLM reranking, project-specific indexes, or remote search
services should provide those through ordinary Harn callbacks, host-backed
tools, or MCP tools and pass the closure as `tool_search.strategy`. The bridge
observes only the resulting `tool_search_query` and `tool_search_result`
transcript events; there is no special `tool_search/query` RPC.

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

## Shell discovery host capability

Shell discovery is a typed `process` host capability so IDEs, TUIs,
headless CLI runs, and cloud workers can share one shell-selection
contract. Harn's standalone fallback exposes the same operations when no
bridge host is attached.

### `process.list_shells`

Called through `host_call("process.list_shells", {})`. The response is:

```json
{
  "shells": [
    {
      "id": "zsh",
      "label": "zsh",
      "path": "/bin/zsh",
      "platform": "darwin",
      "available": true,
      "supports_login": true,
      "supports_interactive": true,
      "default_args": ["-c"],
      "login_args": ["-l", "-c"],
      "source": "env"
    }
  ],
  "default_shell_id": "zsh"
}
```

Windows hosts should distinguish `pwsh`, `powershell`, and `cmd`.
`cmd` uses `["/C"]`; PowerShell variants use
`["-NoProfile", "-Command"]`.

### `process.get_default_shell` / `process.set_default_shell`

`process.get_default_shell` returns the selected shell object for the
session. Stateful hosts may implement `process.set_default_shell` with
`{ "shell_id": "zsh" }`; hosts that keep persistence in editor settings
can omit persistence and still pass the selected shell on command
requests.

### `process.shell_invocation`

`process.shell_invocation` resolves a discovered shell ID or shell object
into executable argv:

```json
{
  "shell_id": "zsh",
  "command": "printf ok",
  "login": false,
  "interactive": false
}
```

Response:

```json
{
  "program": "/bin/zsh",
  "args": ["-c", "printf ok"],
  "command_arg_index": 1,
  "shell": {"id": "zsh"}
}
```

Shell-mode command runners may pass a shell object or a shell ID
resolved through this capability, and otherwise use the selected default
shell. `argv` mode remains preferred for programmatic execution; shell
mode is for user-authored commands and interactive shell semantics. The
normative JSON Schema lives at
`spec/schemas/host-shell-discovery.schema.json`.

## Skill registry

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
