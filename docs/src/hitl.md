# Human in the loop

Harn's human-in-the-loop surface is a set of typed methods on the
`interaction` capability: `harness.interaction.ask_user`,
`.request_approval`, `.dual_control`, and `.escalate_to`. The VM
enforces waiting, timeouts, quorum, escalation, signed receipts, the
event log, and replay determinism.

Use `import "std/hitl"` when you want shared type aliases such as
`ApprovalRecord` or `EscalationHandle`. The methods themselves need no
import — they come with the `Harness` value.

## Call shape: positional arguments plus an options record

Harn has no keyword-argument call syntax. Every HITL primitive takes its
required arguments positionally, with the optional settings gathered into
a trailing options record:

```harn,ignore
const record = harness.interaction.request_approval(
  "deploy production",
  {quorum: 2, reviewers: ["alice", "bob", "carol"]},
)
```

Writing `harness.interaction.request_approval(action: "deploy
production", quorum: 2)` is a parse error (`HARN-PAR-001: expected
expression, found :`), not an alternative form. Note also that a key
the primitive does not recognise is ignored rather than rejected, so a
misspelled option silently takes no effect — check the option names
against the signatures below.

## VM-enforced invariants

The primitives are capability methods, so reaching them at all requires
the `interaction` capability. On top of that:

- **Approval cannot be bypassed by composition.** The result envelope is
  produced by the VM, including a SHA-256 receipt over reviewer
  identity, signing time, and the approval verdict. Forging the
  envelope by constructing a dict literal does not yield valid
  signatures, so downstream consumers can verify provenance.
- **Dual-control quorum requires distinct principals.** The runtime
  deduplicates reviewer IDs before counting approvals; replaying a
  single signer's approval cannot satisfy `n` of `m`.
- **Audit log records every decision.** Each request, response,
  approval, denial, and timeout is appended to the event log on a
  topic determined by the primitive.
- **Replay is deterministic.** Given the same recorded approval
  outcomes, a replay of a script produces the same envelope (including
  the same signatures), enabling reproducible audits.

## Primitives

### `harness.interaction.ask_user<T>(prompt, options?) -> T`

`options` is `{schema?: Schema<T>, timeout?: duration, default?: T}`.

Pause the current dispatch until the host returns a typed response.

- If `schema` is present, the returned value must satisfy it.
- If `default` is present and no schema is supplied, Harn coerces the host
  response toward the default's type when possible.
- `timeout` defaults to 24 hours.
- If the wait times out, Harn returns `default` when present; otherwise it
  throws `HumanTimeoutError`.
- Event log:
  - request: `hitl.question_asked`
  - response: `hitl.response_received`
  - timeout: `hitl.timeout`

```harn
type Choice = {
  environment: "staging" | "prod",
}

const choice: Choice = harness.interaction.ask_user(
  "Where should this deploy?",
  {schema: schema_of(Choice)},
)
```

### `harness.interaction.request_approval(action, options?) -> ApprovalRecord`

Signature: `(action: string, options?: ApprovalRequestOptions) ->
ApprovalRecord`.

```harn,ignore
type ApprovalRequestOptions = {
  detail?: any,
  args?: any,
  quorum?: int,
  reviewers?: list<string>,
  deadline?: duration,
  principal?: string,
  evidence_refs?: list<dict>,
  undo_metadata?: dict,
  capabilities_requested?: list<string>,
}
```

Emit an approval request and wait for a quorum of approving reviewers.

- `quorum` defaults to `1`.
- `deadline` defaults to 24 hours.
- If `reviewers` is omitted, any authorized reviewer may approve.
- `args` is the canonical host-rendered argument payload. When omitted, Harn
  uses `detail` for compatibility with older scripts.
- `principal` defaults to the current dispatch agent or session.
- `evidence_refs`, `undo_metadata`, and `capabilities_requested` are forwarded
  unchanged for hosts that render provenance, undo, or capability UX.
- Denial raises `ApprovalDeniedError`, which scripts can catch with `try`.
- Event log:
  - request: `hitl.approval_requested`
  - responses: `hitl.response_received`
  - approved: `hitl.approval_approved`
  - denied: `hitl.approval_denied`
  - timeout: `hitl.timeout`

`ApprovalRecord` is the shared shape:

```harn
type ApprovalRecord = {
  approved: bool,
  reviewers: list<string>,
  approved_at: string,
  reason: string | nil,
  signatures: list<ApprovalSignature>,
}

type ApprovalSignature = {
  reviewer: string,
  signed_at: string,
  signature: string,
}
```

Each counted approval contributes one signature receipt. Hosts may provide a
signature directly; otherwise the VM records a deterministic receipt signature
over the request id, reviewer, approval decision, reason, and signed timestamp.

`ApprovalRequest` is the canonical host-rendered request shape used by stdlib
HITL, bridge permission prompts, and external approval inboxes:

```harn
type ApprovalRequest = {
  id: string,
  action: string,
  args: any,
  principal: string,
  requested_at: string,
  deadline: string | nil,
  approvers_required: int,
  evidence_refs: list<dict>,
  undo_metadata: dict | nil,
  capabilities_requested: list<string>,
}
```

For stdlib HITL events, the durable request envelope stores this under
`payload.approval_request` and also mirrors the same fields at `payload.id`,
`payload.action`, `payload.args`, and so on. Legacy `detail`, `quorum`,
`reviewers`, and `deadline_ms` fields remain present so older hosts continue to
render and resolve existing flows.

### `harness.interaction.dual_control<T>(n, m, action, approvers?) -> T`

Signature: `(n: int, m: int, action: fn() -> T, approvers?: list<string>) -> T`.

Run a closure only after `n` approvals out of `m` named approvers.

- Typical destructive-operation pattern: `harness.interaction.dual_control(2, 3, { -> ... }, ["alice", "bob", "carol"])`
- The closure does not run until quorum is satisfied.
- Denial raises `ApprovalDeniedError`.
- Event log:
  - request: `hitl.dual_control_requested`
  - responses: `hitl.response_received`
  - approved: `hitl.dual_control_approved`
  - denied: `hitl.dual_control_denied`
  - executed: `hitl.dual_control_executed`
  - timeout: `hitl.timeout`

### `harness.interaction.escalate_to(role: string, reason: string) -> EscalationHandle`

Raise the current dispatch to a higher-trust role and block until the host
accepts the escalation.

- The request is persisted before the dispatch pauses.
- The request payload includes the active capability policy when one is
  installed, so hosts can route or resolve the requested role against the same
  capability ceiling the VM is enforcing.
- The host resolves it by appending an acceptance event.
- If nobody accepts it, the dispatch remains paused until a host or operator
  resumes it explicitly.
- Event log:
  - request: `hitl.escalation_issued`
  - acceptance: `hitl.escalation_accepted`

`EscalationHandle` is the shared shape:

```harn
type EscalationHandle = {
  request_id: string,
  role: string,
  reason: string,
  trace_id: string,
  status: string,
  accepted_at: string | nil,
  reviewer: string | nil,
}
```

### `harness.interaction.hitl_pending(filters?) -> list<HitlPendingRequest>`

`filters` is a `HitlPendingFilters` record.

Read the active event log and return pending HITL requests as typed rows.

- Returns `[]` when no event log is attached.
- Reads and merges `hitl.questions`, `hitl.approvals`, `hitl.dual_control`,
  and `hitl.escalations`.
- Filters are all optional:
  - `since` / `until`: inclusive RFC3339 timestamp bounds
  - `kinds`: subset of `"question" | "approval" | "dual_control" | "escalation"`
  - `agent`: exact agent id match
  - `limit`: defaults to 500, capped at 5000
- Results are sorted newest first and omit requests that already reached a
  terminal HITL event.

`HitlPendingRequest` is the shared row shape:

```harn
type HitlPendingRequest = {
  request_id: string,
  request_kind: HitlRequestKind,
  agent: string,
  prompt: string,
  trace_id: string,
  timestamp: string,
  approvers: list<string>,
  metadata: dict,
}
```

## Event topics

HITL records are written to dedicated durable topics:

- `hitl.questions`
- `hitl.approvals`
- `hitl.dual_control`
- `hitl.escalations`

These append through the normal event-log path, so they get the same
crash-safety guarantees as trigger dispatch records.

## Host contract

When a builtin opens a HITL wait, Harn emits a bridge notification:

- `harn.hitl.requested`

For approval and dual-control requests, notification params are the durable
request envelope and include `payload.approval_request` with the canonical
`ApprovalRequest` object. Hosts should render that object instead of inventing
host-specific approval shapes.

Hosts resolve pending requests with the JSON-RPC method:

- `harn.hitl.respond`

The response payload includes the `request_id` plus the relevant fields for
that request kind:

- questions: `answer`
- approvals / dual control: `approved`, `reviewer`, optional `reason`
- escalations: `accepted`, `reviewer`, optional `reason`

ACP and MCP both expose `harn.hitl.respond`. The orchestrator CLI also exposes
manual escalation resume via `harn orchestrator resume <request_id>`.

## Replay semantics

Replay is event-log-driven.

- Live dispatch: the host provides responses through `harn.hitl.respond`.
- Replay: the VM reads the previously recorded `hitl.response_received` or
  `hitl.escalation_accepted` events instead of consulting a live host.

This makes `trigger_replay(...)` and `harn trigger replay <event-id>` replay-safe
for HITL flows as long as the original run recorded the HITL response events.
Approval reviewer identities, signed timestamps, and receipt signatures are
reused from those events during replay.

## Patterns

Catch denials explicitly:

```harn
const result = try {
  harness.interaction.request_approval(
    "deploy production",
    {quorum: 2, reviewers: ["alice", "bob"]},
  )
}
if is_err(result) && unwrap_err(result).name == "ApprovalDeniedError" {
  harness.stdio.log("deployment denied")
}
```

Gate a destructive step behind dual control:

```harn,ignore
const deleted = harness.interaction.dual_control(
  2,
  3,
  { ->
    harness.fs.delete("prod.db")
    return true
  },
  ["alice", "bob", "carol"],
)
```
