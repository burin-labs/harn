# Human In The Loop

Harn's human-in-the-loop surface is a typed stdlib, not special syntax.
Scripts call builtins such as `ask_user(...)` and `request_approval(...)`,
while the VM enforces the waiting, timeout, quorum, escalation, event-log,
and replay behavior.

Use `import "std/hitl"` when you want shared type aliases such as
`ApprovalRecord` or `EscalationHandle`. The builtins themselves are global.

## Primitives

### `ask_user<T>(prompt: string, options?: {schema?: Schema<T>, timeout?: duration, default?: T}) -> T`

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

let choice: Choice = ask_user(
  "Where should this deploy?",
  {schema: schema_of(Choice)},
)
```

### `request_approval(action: string, options?: {detail?: any, quorum?: int, reviewers?: list<string>, deadline?: duration}) -> ApprovalRecord`

Emit an approval request and wait for a quorum of approving reviewers.

- `quorum` defaults to `1`.
- `deadline` defaults to 24 hours.
- If `reviewers` is omitted, any authorized reviewer may approve.
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

### `dual_control<T>(n: int, m: int, action: fn() -> T, approvers?: list<string>) -> T`

Run a closure only after `n` approvals out of `m` named approvers.

- Typical destructive-operation pattern: `dual_control(2, 3, { -> ... }, ["alice", "bob", "carol"])`
- The closure does not run until quorum is satisfied.
- Denial raises `ApprovalDeniedError`.
- Event log:
  - request: `hitl.dual_control_requested`
  - responses: `hitl.response_received`
  - approved: `hitl.dual_control_approved`
  - denied: `hitl.dual_control_denied`
  - executed: `hitl.dual_control_executed`
  - timeout: `hitl.timeout`

### `escalate_to(role: string, reason: string) -> EscalationHandle`

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

### `hitl_pending(filters?: HitlPendingFilters) -> list<HitlPendingRequest>`

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

## Event Topics

HITL records are written to dedicated durable topics:

- `hitl.questions`
- `hitl.approvals`
- `hitl.dual_control`
- `hitl.escalations`

These append through the normal event-log path, so they get the same
crash-safety guarantees as trigger dispatch records.

## Host Contract

When a builtin opens a HITL wait, Harn emits a bridge notification:

- `harn.hitl.requested`

Hosts resolve pending requests with the JSON-RPC method:

- `harn.hitl.respond`

The response payload includes the `request_id` plus the relevant fields for
that request kind:

- questions: `answer`
- approvals / dual control: `approved`, `reviewer`, optional `reason`
- escalations: `accepted`, `reviewer`, optional `reason`

ACP and MCP both expose `harn.hitl.respond`. The orchestrator CLI also exposes
manual escalation resume via `harn orchestrator resume <request_id>`.

## Replay Semantics

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
let result = try {
  request_approval("deploy production", {quorum: 2, reviewers: ["alice", "bob"]})
}
if is_err(result) && unwrap_err(result).name == "ApprovalDeniedError" {
  println("deployment denied")
}
```

Gate a destructive step behind dual control:

```harn,ignore
let deleted = dual_control(2, 3, {
  delete_file("prod.db")
  return true
}, ["alice", "bob", "carol"])
```
