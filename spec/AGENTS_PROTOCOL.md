# Harn Agents Protocol v1

The Harn Agents Protocol is the public wire contract for managed Harn agents.
It lets clients, hosts, and third-party Harness implementations create
sessions, submit tasks, stream agent activity, exchange artifacts, verify audit
receipts, and replay durable execution history without depending on Harn Cloud
internals.

This document is the narrative specification for protocol version 1. It is
authoritative for resource semantics, lifecycle rules, transport behavior,
authentication, idempotency, event names, error categories, and conformance
levels. The REST OpenAPI document, receipt wire schema, replay API contract, and
agents conformance suite are specified in sibling artifacts and must not
weaken the requirements in this document.

Canonical publication URL:

```text
https://harnlang.com/spec/agents-protocol/v1
```

## Status

- Protocol family: Harn Agents Protocol
- Version: v1
- Version header: `Harn-Agents-Protocol-Version`
- Initial version value: `agents-protocol-2026-04-25`
- Stability: draft until the v1 conformance suite lands
- Reference implementation: `burin-labs/harn-cloud`
- Open source specification home: `burin-labs/harn`

## Design Goals

The protocol is designed around these constraints:

- Harn owns orchestration, policy, transcript lifecycle, replay semantics,
  receipts, evals, delegated-worker lineage, and mutation-session audit data.
- Hosts own user experience, approval surfaces, concrete file mutations,
  app-specific state, and undo/redo semantics.
- The same logical agent run should be observable over REST polling, SSE
  streaming, and WebSocket sessions.
- A third-party Harness should be able to pass conformance without
  Harn-Cloud-specific behavior.
- Durable resources should be reconstructable from append-only events.
- Receipts prove what happened without exposing full private traces by default.

## Normative Language

The key words "MUST", "MUST NOT", "SHOULD", "SHOULD NOT", and "MAY" are
normative when written in all caps.

Fields named `id` are stable resource identifiers inside a Harness. A Harness
MUST NOT recycle an identifier for a different resource. Unless a field says
otherwise, timestamps are RFC3339 UTC strings and objects MAY include unknown
extension fields under `metadata`.

## Compatibility With Existing Harn Surfaces

This protocol reuses existing Harn concepts instead of redefining them:

- A2A task semantics provide the base task lifecycle.
- ACP session semantics inform WebSocket session streaming and host mediation.
- The Harn EventLog model provides append-only event identity and replay
  ordering.
- Persona manifests define durable agent roles and their policy surface.
- OpenTrustGraph records define portable autonomy and approval audit evidence.
- Transcript architecture defines message, event, and asset layering.

Existing Harn adapters MAY expose compatibility aliases, such as A2A
`cancelled`, but this protocol's canonical state spelling is `CANCELED`.

## Versioning

Every request except public discovery endpoints MUST include:

```text
Harn-Agents-Protocol-Version: agents-protocol-2026-04-25
```

Servers MUST reject unsupported protocol versions with `426 Upgrade Required`
and error code `unsupported_protocol_version`. The response SHOULD list
supported versions.

Breaking changes require a new date-stamped version value. Non-breaking
changes MAY be added to an existing version when they only add optional fields,
new event kinds, new resource links, new error details, or new enum values that
old clients can ignore safely.

Clients MUST ignore unknown object fields. Clients MUST NOT treat an unknown
enum value as success. When an unknown enum value appears, clients SHOULD keep
the raw value for display and handle the resource as an indeterminate active or
failed state according to the field's context.

## Resource Model

All resources share this envelope shape when represented as top-level REST
objects:

```json
{
  "id": "resource_01J...",
  "object": "task",
  "created_at": "2026-04-25T19:00:00Z",
  "updated_at": "2026-04-25T19:00:02Z",
  "metadata": {}
}
```

`object` is a lower-snake-case discriminator. Resource-specific sections below
list required fields in addition to the common envelope.

### Persona

A Persona is a durable operational role. It binds a human-readable
responsibility to an entry workflow, tool and capability policy, autonomy tier,
budget limits, receipt requirements, and handoff targets.

Required fields:

- `id`: stable persona id.
- `name`: display name.
- `version`: persona contract version.
- `entry_workflow`: workflow or module entrypoint.
- `description`: operational responsibility.
- `autonomy_tier`: one of `shadow`, `suggest`, `act_with_approval`,
  `act_auto`.
- `receipt_policy`: one of `required`, `optional`, `disabled`.

Optional fields include `tools`, `capabilities`, `triggers`, `schedules`,
`handoffs`, `context_packs`, `evals`, `owner`, `model_policy`,
`rollout_policy`, and `quota_id`.

A Persona is policy. It is not merely a prompt. Servers MUST enforce persona
capability, autonomy, receipt, and quota policies on every Task that names the
Persona.

### Workspace

A Workspace is the isolation boundary for files, tools, secrets, sessions,
branches, event logs, and run records.

Required fields:

- `id`: stable workspace id.
- `name`: display name.
- `root`: implementation-defined logical root or URI.
- `default_branch_id`: Branch id or `null`.

Optional fields include `host`, `repository`, `tenant_id`, `capabilities`,
`connectors`, and `quota_id`.

Workspace identifiers MUST be included on stateful resources unless the
resource is explicitly global to the Harness.

### Session

A Session is a conversational and execution continuity boundary. It owns
transcript history, lifecycle, branch lineage, pending input requests, and
stream cursors.

Required fields:

- `id`: stable session id.
- `workspace_id`: owning workspace.
- `state`: one of `ACTIVE`, `IDLE`, `PAUSED`, `CLOSED`, `FAILED`.
- `transcript`: transcript summary or link.

Optional fields include `persona_id`, `root_session_id`, `parent_session_id`,
`branch_id`, `last_event_id`, `summary`, and `expires_at`.

Servers MUST preserve message ordering inside a Session. Servers MAY compact
older transcript content, but compaction MUST preserve enough event and
artifact references for audit and replay.

### Task

A Task is a unit of agent work submitted by a user, connector, automation, or
another agent.

Required fields:

- `id`: stable task id.
- `session_id`: owning Session.
- `workspace_id`: owning Workspace.
- `status`: Task lifecycle state.
- `input`: initial Message or structured task payload.
- `created_by`: actor id or connector id.

Optional fields include `persona_id`, `branch_id`, `parent_task_id`,
`assigned_agent_id`, `receipt_id`, `outcome_id`, `quota_id`, `started_at`,
`completed_at`, `canceled_at`, and `failure`.

Tasks MUST be durable once accepted. A server that accepts a Task and then
crashes MUST recover it as `SUBMITTED`, `WORKING`, `FAILED`, or `CANCELED`
after restart.

### Branch

A Branch is a lineage marker for alternate session, workspace, or task
execution paths. Branches MAY correspond to Git worktrees, transcript forks,
planner branches, or hosted sandbox copies.

Required fields:

- `id`: stable branch id.
- `workspace_id`: owning Workspace.
- `kind`: one of `workspace`, `session`, `task`, `sandbox`.
- `base_ref`: implementation-defined parent ref.

Optional fields include `parent_branch_id`, `session_id`, `task_id`,
`worktree_uri`, `created_by`, and `merged_into`.

Servers MUST expose enough Branch lineage for a client to understand whether a
mutation or artifact came from the main path or an alternate execution path.

### Message, Part, And Artifact

A Message is a conversational turn or protocol-visible agent update.

Required Message fields:

- `id`: stable message id.
- `role`: one of `user`, `assistant`, `system`, `tool`, `agent`.
- `parts`: ordered Part list.
- `created_at`: timestamp.

A Part is one block of message content. Required Part fields:

- `type`: content discriminator.
- `visibility`: one of `public`, `internal`, `receipt_only`.

Core Part types:

- `text`: `{type, text, visibility}`
- `json`: `{type, value, visibility}`
- `tool_call`: `{type, tool_call_id, name, input, visibility}`
- `tool_result`: `{type, tool_call_id, output, status, visibility}`
- `artifact_ref`: `{type, artifact_id, visibility}`
- `file_ref`: `{type, uri, mime_type, visibility}`
- `image_ref`: `{type, artifact_id, mime_type, visibility}`

An Artifact is durable, non-prompt content referenced by messages, outcomes, or
receipts.

Required Artifact fields:

- `id`: stable artifact id.
- `kind`: one of `file`, `patch`, `image`, `log`, `diff`, `receipt`,
  `snapshot`, `dataset`, `other`.
- `mime_type`: media type or `application/octet-stream`.
- `uri`: retrievable URI or `null` when access is mediated by a resource API.
- `visibility`: one of `public`, `internal`, `receipt_only`.
- `sha256`: content hash when bytes are stable.

Servers SHOULD reference large payloads as Artifacts instead of inlining them
in Message parts.

### AgentCard

The Harn Agents API `AgentCard` is a Harn resource envelope that advertises
agent endpoints, policy, and capabilities. It is not itself an A2A AgentCard.
When an implementation also wants to expose A2A discovery, it MUST place the
A2A-compatible card in `a2a_agent_card` instead of mixing A2A camelCase fields
into the Harn envelope.

Required fields:

- `id`: stable card id.
- `name`: display name.
- `description`: agent description.
- `protocol_version`: supported Harn Agents Protocol version.
- `interfaces`: available transport interfaces.
- `skills`: callable skills or exported functions.
- `a2a_agent_card`: nested A2A AgentCard projection.

Optional fields include `persona_ids`, `capabilities`, `auth_schemes`,
`receipt_policy`, `quotas`, `provider`, `public_url`, and `signature`.

The nested `a2a_agent_card` MUST use A2A field names such as `version`,
`url`, `capabilities`, `supportedInterfaces`, `securitySchemes`,
`securityRequirements`, `defaultInputModes`, `defaultOutputModes`, and
`skills`. SDK generators should expose the REST resource as `HarnAgentCard`
and the nested projection as `A2aAgentCard`; clients should pass only the
nested object to A2A SDKs.

For draft compatibility, servers MAY continue to emit the legacy top-level
Harn fields `interfaces`, `auth_schemes`, `receipt_policy`, and `public_url`.
New clients MUST prefer `a2a_agent_card` for A2A interop and treat those legacy
top-level fields as Harn policy/discovery metadata only.

Signed cards MUST state their signature algorithm and key id. Clients MUST NOT
trust unsigned card metadata for authorization decisions.

### Event

An Event is an append-only fact about a resource. Events are the source of
truth for streaming and replay.

Required fields:

- `id`: monotonic event id within the stream.
- `event`: event kind.
- `resource`: `{object, id}` pointer.
- `created_at`: timestamp.
- `sequence`: per-resource sequence number.
- `payload`: event-specific object.

Optional fields include `trace_id`, `span_id`, `session_id`, `task_id`,
`workspace_id`, `actor`, `idempotency_key`, `previous_event_id`, and
`receipt_id`.

Servers MUST emit Events in causal order per resource. Global ordering MAY be
best-effort unless a transport explicitly promises a single stream order.

### Receipt

A Receipt is a portable proof summary for a Task, Outcome, Event, Artifact,
approval, tool use, or replay segment. Receipts are not full traces.

Required fields:

- `schema`: receipt format discriminator. Current value:
  `receipt-2026-04-25`.
- `receipt_id`: stable receipt id.
- `subject`: `{object, id}` pointer.
- `issuer`: Harness identity.
- `issued_at`: timestamp.
- `identifiers`: tenant, persona, workspace, session, task, branch, and trace
  identifiers known for the run.
- `lifecycle`: lifecycle timestamps and final state.
- `trust`: autonomy tier at start and end.
- `autonomy_budget`: consumed autonomy budget.
- `replay_input`: replay material references sufficient for deterministic
  replay when available.
- `model_route`: chosen model, alternatives considered, and route-policy
  reason.
- `cost`: total cost and per-provider breakdown.
- `side_effects`: file-system writes, network egress, tool calls, and A2A
  handoffs.
- `final_artifacts`: final artifact references.
- `chain`: previous receipt hash, current receipt hash, and optional Merkle
  root.

Optional fields include `approvals`, `redactions`, `signatures`, and
`metadata`.

The normative JSON Schema lives in
`agents-protocol-receipts/schemas/receipt-2026-04-25.schema.json`. The Agents
Protocol OpenAPI `Receipt` component MUST reference that schema instead of
duplicating it.

### Memory

Memory is durable knowledge that a Persona or Session may read in later Tasks.

Required fields:

- `id`: stable memory id.
- `scope`: one of `persona`, `session`, `workspace`, `tenant`.
- `owner_id`: id for the selected scope.
- `content`: text, JSON, or artifact reference.
- `provenance`: source event, task, or actor.

Optional fields include `expires_at`, `embedding_ref`, `visibility`,
`redaction_policy`, and `confidence`.

Servers MUST separate Memory from transcript history. A transcript replay MUST
not silently include Memory unless the original run also had access to it or
the replay request explicitly opts into refreshed Memory.

### Vault

A Vault stores secrets and protected credentials available to Personas,
Connectors, or Tasks under policy.

Required fields:

- `id`: stable vault id.
- `workspace_id`: owning Workspace or `null` for tenant-global vaults.
- `provider`: vault backend.
- `capabilities`: allowed secret operations.

Secret values MUST NOT be returned by default resource reads. Events and
receipts MUST redact secret material. Tool calls MAY receive secret-derived
credentials only through policy-controlled host or connector execution.

### Connector

A Connector normalizes external provider activity into Tasks, Events, or
Messages.

Required fields:

- `id`: stable connector id.
- `provider`: provider name.
- `workspace_id`: owning Workspace.
- `status`: one of `ACTIVE`, `PAUSED`, `FAILED`, `DISABLED`.
- `event_kinds`: normalized events it can emit.

Optional fields include `dedupe_policy`, `auth`, `webhook`, `polling`,
`target_persona_id`, and `target_session_id`.

Connectors MUST provide stable dedupe keys when the upstream provider exposes
delivery ids or equivalent event identities.

### Skill

A Skill is a callable or loadable capability exposed by a Persona, AgentCard,
host, connector, or Harn package.

Required fields:

- `id`: stable skill id.
- `name`: display name or callable name.
- `description`: short description.
- `input_schema`: JSON Schema object or `null`.
- `output_schema`: JSON Schema object or `null`.

Optional fields include `source`, `version`, `capabilities`,
`requires_approval`, `deprecated`, and `metadata`.

When a Skill maps to an exported Harn `pub fn`, its schema MUST be derived from
the Harn type surface or be explicitly declared by the serving adapter.

### Outcome

An Outcome is the terminal user-visible result of a Task.

Required fields:

- `id`: stable outcome id.
- `task_id`: source Task.
- `status`: one of `SUCCEEDED`, `FAILED`, `CANCELED`.
- `summary`: concise result.

Optional fields include `messages`, `artifacts`, `handoffs`,
`receipt_id`, `failure`, `cost`, and `metrics`.

A completed Task SHOULD have exactly one Outcome. Failed and canceled Tasks MAY
have Outcomes when the server can produce a useful summary.

### Quota

A Quota is a policy resource for spend, tokens, concurrency, runtime, or
rate-limit ceilings.

Required fields:

- `id`: stable quota id.
- `scope`: one of `persona`, `workspace`, `tenant`, `organization`.
- `limits`: object containing one or more numeric limits.
- `usage`: current usage snapshot.

Optional fields include `reset_at`, `hard_limit`, `soft_limit`,
`exhaustion_reason`, and `last_receipt_id`.

Servers MUST check applicable hard quotas before starting work that can
consume meaningful compute, model tokens, external API calls, or money.

## Transports

The protocol defines three transport profiles over the same resource model:

- REST for request/response control and resource retrieval.
- Server-Sent Events for ordered one-way event streams.
- WebSocket for bidirectional interactive sessions and host mediation.

Servers MAY implement a subset of transports according to their conformance
level. A server that advertises a transport in an AgentCard MUST implement that
transport according to this section.

### REST

REST endpoints use HTTPS and JSON. The OpenAPI 3.1 artifact defines the exact
paths and schemas. This narrative spec requires the REST surface to cover:

- discovery and AgentCard retrieval
- Persona, Workspace, Session, Task, Branch, Message, Artifact, Event,
  Receipt, Memory, Connector, Skill, Outcome, and Quota reads
- Session creation, close, fork, and message append
- Task submit, get, cancel, and list
- artifact upload or registration
- event range reads

REST writes that create or mutate resources MUST accept `Idempotency-Key` when
the method is not naturally idempotent.

### Server-Sent Events

SSE streams deliver Events as UTF-8 `text/event-stream`.

Each SSE frame MUST include:

- `id`: Event id as a decimal or opaque string cursor.
- `event`: Event kind.
- `data`: JSON Event object.

Clients resume by sending `Last-Event-ID`. Servers SHOULD replay missed events
from the durable event log when possible. If the cursor is too old or
unavailable, servers MUST return a clear error event and close the stream
instead of pretending the stream is continuous.

SSE is one-way. Client input, approval, or cancellation must use REST or
WebSocket.

### WebSocket

WebSocket sessions use `wss://` except for loopback or explicitly trusted
development deployments. Frames are JSON objects.

The WebSocket profile MUST support:

- client-to-server user messages
- client-to-server task cancellation
- server-to-client Event frames
- server-to-client input or authorization requests
- optional host-mediated tool or approval requests
- cursor-based resume

The server SHOULD send ping frames or protocol heartbeat events often enough
for clients to detect broken connections. A resumed WebSocket session MUST
mark replayed events so clients can distinguish historical delivery from live
activity.

## Authentication And Authorization

The protocol defines two baseline authentication schemes.

### API Key

API key authentication uses:

```text
Authorization: Bearer <api-key>
```

Servers MUST bind API keys to an actor and authorization policy. API keys MUST
NOT be logged in Events, Receipts, or error details.

### OAuth2 Client Credentials

Machine-to-machine clients MAY use OAuth2 client credentials. Tokens are sent
with the same bearer header:

```text
Authorization: Bearer <access-token>
```

Servers MUST validate issuer, audience, expiry, and required scopes. Token
scopes SHOULD map to resource actions such as `sessions:read`, `tasks:write`,
`events:read`, `artifacts:write`, and `receipts:read`.

### Authorization Rules

Authentication identifies the caller. Authorization decides whether the caller
may perform an action.

Servers MUST evaluate at least:

- actor permissions
- Workspace or tenant membership
- Persona policy
- tool and host capability policy
- Vault access policy
- Quota policy
- approval requirements for side effects

Authorization failures MUST NOT leak secret values or hidden resource
existence beyond what the actor is allowed to know.

## Idempotency

Clients SHOULD send `Idempotency-Key` on all non-idempotent REST writes and MAY
send it on WebSocket command frames.

The idempotency key scope is:

```text
actor + workspace_id + method + canonical target + Idempotency-Key
```

Servers MUST persist the first completed response for a key for at least 24
hours. Retries with the same key and equivalent request body MUST return the
same resource id and a semantically equivalent response. Retries with the same
key and a different body MUST fail with `409 Conflict` and error code
`idempotency_key_reused`.

If a request is still running, a retry MAY return the in-flight Task or a
`202 Accepted` response pointing at it. A server MUST NOT create duplicate
Tasks, Messages, Artifacts, or approval actions for equivalent retried writes.

Connectors SHOULD map provider delivery ids to idempotency keys or dedupe keys
when submitting Tasks.

## Task Lifecycle

Task states are uppercase on the Harn Agents Protocol wire:

- `SUBMITTED`
- `WORKING`
- `INPUT_REQUIRED`
- `AUTH_REQUIRED`
- `COMPLETED`
- `FAILED`
- `CANCELED`

Allowed transitions:

| From | To |
| --- | --- |
| `SUBMITTED` | `WORKING`, `CANCELED`, `FAILED` |
| `WORKING` | `INPUT_REQUIRED`, `AUTH_REQUIRED`, `COMPLETED`, `FAILED`, `CANCELED` |
| `INPUT_REQUIRED` | `WORKING`, `FAILED`, `CANCELED` |
| `AUTH_REQUIRED` | `WORKING`, `FAILED`, `CANCELED` |
| `COMPLETED` | terminal |
| `FAILED` | terminal |
| `CANCELED` | terminal |

`SUBMITTED` means the Harness accepted the Task durably. `WORKING` means an
agent, workflow, or queued worker started processing. `INPUT_REQUIRED` means
the Task is blocked waiting for user or caller input. `AUTH_REQUIRED` means the
Task is blocked waiting for authorization, approval, OAuth, or secret
connection setup. `COMPLETED` means the Task reached a successful terminal
Outcome. `FAILED` means the Task reached an unsuccessful terminal state.
`CANCELED` means a caller, policy, host, or shutdown path canceled the Task.

Servers MUST emit a Task state event for every lifecycle transition. Servers
MUST reject client attempts to move a terminal Task back to a non-terminal
state. Cancellation is cooperative: a Task MAY finish before a cancel request
is observed, but once the server records `CANCELED`, no later success Outcome
may replace it.

## Event Taxonomy

Event names are lower-snake or dotted names. Dotted names group events by
actor and subsystem. Servers MAY add vendor events, but vendor events SHOULD
use a reverse-DNS or `vendor.` prefix.

### User Events

- `user.message`: user Message appended to a Session.
- `user.input_submitted`: requested input supplied.
- `user.tool_confirmation`: user approved, denied, or modified a tool action.
- `user.cancel_requested`: user requested Task cancellation.

### Agent Events

- `agent.message`: assistant or agent Message appended.
- `agent.reasoning_summary`: optional redacted reasoning summary.
- `agent.tool_use`: model-requested tool call.
- `agent.tool_result`: tool result observed by the agent.
- `agent.mcp_tool_use`: MCP tool call issued by the agent.
- `agent.handoff`: handoff to another Persona, Task, or remote agent.
- `agent.memory_read`: Memory read by the run.
- `agent.memory_write`: Memory written by the run.

Tool event payloads SHOULD align with common model provider tool-use shapes,
including Anthropic-style tool ids and input/result pairing where practical.
Events MUST NOT expose hidden chain-of-thought. Summaries and receipts may
describe decisions without revealing private reasoning.

### Task Events

- `task.submitted`
- `task.started`
- `task.input_required`
- `task.auth_required`
- `task.completed`
- `task.failed`
- `task.canceled`
- `task.status_changed`

### Session Events

- `session.created`
- `session.closed`
- `session.message_appended`
- `session.compacted`
- `session.thread_forked`
- `session.thread_merged`
- `session.replayed`

### Branch Events

- `branch.created`
- `branch.updated`
- `branch.merged`
- `branch.abandoned`

### Artifact Events

- `artifact.created`
- `artifact.updated`
- `artifact.linked`
- `artifact.redacted`

### Tool And Span Events

- `span.started`
- `span.updated`
- `span.completed`
- `span.failed`
- `tool.requested`
- `tool.approval_required`
- `tool.approved`
- `tool.denied`
- `tool.completed`
- `tool.failed`

Span events provide timing and hierarchy. Tool events provide user-facing tool
semantics. A server MAY emit both for the same action when it preserves
correlation ids.

### Connector Events

- `connector.delivery_received`
- `connector.delivery_deduped`
- `connector.normalized`
- `connector.task_submitted`
- `connector.failed`

### Receipt And Replay Events

- `receipt.issued`
- `receipt.verified`
- `receipt.verification_failed`
- `replay.started`
- `replay.event_replayed`
- `replay.completed`
- `replay.failed`

## Error Taxonomy

Errors use HTTP status codes plus machine-readable error codes.

Error response shape:

```json
{
  "error": {
    "code": "invalid_request",
    "message": "Missing required field: session_id",
    "type": "request_error",
    "param": "session_id",
    "request_id": "req_01J...",
    "details": {}
  }
}
```

Core error types:

- `request_error`: malformed or semantically invalid request.
- `auth_error`: authentication failed or token is invalid.
- `permission_error`: authenticated caller lacks permission.
- `not_found_error`: requested resource is not visible to the caller.
- `conflict_error`: current state conflicts with the request.
- `rate_limit_error`: rate or quota limit blocked the request.
- `runtime_error`: agent, workflow, tool, or provider execution failed.
- `upstream_error`: connector, model provider, host, or tool backend failed.
- `server_error`: unexpected Harness failure.

Core error codes:

| HTTP | Code | Meaning |
| --- | --- | --- |
| 400 | `invalid_request` | Request shape or value is invalid. |
| 400 | `invalid_state_transition` | Requested lifecycle transition is not allowed. |
| 401 | `unauthenticated` | Authentication is missing or invalid. |
| 403 | `permission_denied` | Actor is not allowed to perform the action. |
| 404 | `resource_not_found` | Resource is absent or hidden from caller. |
| 409 | `conflict` | Request conflicts with current resource state. |
| 409 | `idempotency_key_reused` | Same key was used with a different body. |
| 410 | `cursor_expired` | Event or replay cursor is no longer available. |
| 413 | `payload_too_large` | Request or artifact exceeds configured limit. |
| 422 | `policy_violation` | Persona, quota, approval, or capability policy blocked work. |
| 423 | `resource_locked` | Resource is locked by another branch, worker, or lease. |
| 426 | `unsupported_protocol_version` | Version header is absent or unsupported. |
| 429 | `rate_limited` | Rate limit or quota throttled the request. |
| 499 | `client_closed_request` | Client disconnected before completion. |
| 500 | `internal_error` | Unexpected server error. |
| 502 | `upstream_unavailable` | Upstream dependency failed or timed out. |
| 503 | `service_unavailable` | Harness is unavailable or draining. |
| 504 | `deadline_exceeded` | Request deadline expired. |

Errors caused by agent work SHOULD also produce Task or Span events so the
failure is visible in streams and receipts.

## Receipt Wire Format

The receipt-format sibling artifact defines the normative receipt envelope,
canonicalization, hash inputs, signatures, redaction rules, verification
algorithm, JSON Schema, fixtures, and optional CBOR archive encoding.

Receipt JSON uses the `receipt-2026-04-25` schema marker. Producers MUST
canonicalize receipt JSON with RFC 8785 before hashing or signing. The
`chain.receipt_hash` value is computed over the canonical receipt with
`chain.receipt_hash` and `signatures` removed. The stored value uses the
`sha256:` prefix.

Task, Outcome, Event, and Artifact objects MAY reference receipts by id.
Clients MUST NOT infer cryptographic validity from the presence of a
`receipt_id`; they must call the receipt verification surface when available.

## Replay Contract

The replay-as-API sibling spec defines exact REST paths and replay fixtures.
This narrative spec requires:

- `POST /v1/tasks/{task_id}/replay` creates a new Task by replaying the source
  Task EventLog. The new Task MUST expose the source Task as `parent_task_id`.
- Replay requests support `exact`, `with_overrides`, and `from_checkpoint`
  modes. Overrides MUST be keyed deterministic substitutions for recorded
  nondeterministic dependencies such as model responses, MCP tool returns,
  secret values, clock reads, and host facts.
- Events are durable enough to replay a Session or Task history within the
  server's advertised retention window.
- Replayed stream events are marked as replayed.
- Replay MUST preserve original event ids where possible and MUST expose a
  replay cursor when ids are remapped.
- Replay MUST NOT silently include refreshed Memory, changed secrets, or new
  host facts unless the replay request explicitly opts into non-deterministic
  refresh.
- Applied substitutions MUST be recorded as Receipt deltas that identify the
  override key, original event or material path, and before/after hashes when
  available.
- Replay failures MUST identify the first unavailable event, artifact, memory,
  receipt, or host dependency that prevents replay.

## Privacy And Redaction

Servers MUST distinguish public content, internal content, and receipt-only
content. Hidden model reasoning, secrets, bearer tokens, private keys, raw
OAuth credentials, and unapproved host data MUST NOT appear in public Message
parts, Events, or Receipts.

Artifact and Event redaction MUST preserve enough structure for clients to
understand that content existed. Prefer redacted descriptors over deletion when
audit continuity matters.

## Conformance Levels

Conformance is cumulative unless a level says otherwise.

### Core

Core implementations support:

- version negotiation
- API key authentication
- REST AgentCard, Session, Task, Message, Event, Artifact, Outcome reads
- Task submit, get, cancel
- Task lifecycle state machine
- `Idempotency-Key` for Task and Message creation
- SSE Task or Session event streaming
- core error envelope and error codes

### Extended

Extended implementations support all Core requirements plus:

- OAuth2 client credentials
- Persona, Workspace, Branch, Connector, Skill, Memory, Vault, and Quota
  resources
- WebSocket interactive sessions
- host-mediated input and authorization requests
- artifact upload or registration
- connector delivery dedupe behavior
- AgentCard signatures

### Receipts

Receipts implementations support all Core requirements plus:

- Receipt resources
- receipt references from Tasks, Outcomes, Events, and Artifacts
- receipt verification API
- receipt redaction semantics
- receipt conformance fixtures

This level can be implemented with Core or Extended. A Core+Receipts server is
valid when it supports receipts but not WebSocket or the full management
resource set.

### Replay

Replay implementations support all Core requirements plus:

- event range reads with durable cursors
- replay API for Sessions and Tasks
- replayed event markers
- deterministic replay fixtures
- explicit failure reporting for unavailable replay dependencies

Replay SHOULD be combined with Receipts for production Harnesses, but the
levels remain separate so lightweight test Harnesses can validate replay
without implementing cryptographic receipts.

## Implementation Notes

Harn implementations should keep protocol adapters thin. REST, SSE, WebSocket,
A2A, ACP, and MCP surfaces should map into the same dispatch, policy,
transcript, EventLog, and receipt core wherever possible.

When behavior differs by transport, it should be because the transport has a
different interaction shape, not because it changes authorization, replay,
quota, approval, or audit semantics.
