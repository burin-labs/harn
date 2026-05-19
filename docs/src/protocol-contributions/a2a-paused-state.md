# A2A RFC: explicit `PAUSED` task state + `tasks/pause` / `tasks/resume`

**Upstream repo:** [a2aproject/A2A][a2a]
**Status:** Draft (not yet filed upstream).
**Authors:** Burin Labs
**Reference impl:** `harn-vm` cooperative suspend primitive
([`crates/harn-vm/src/stdlib/agents.rs`][agents-rs] —
`__host_worker_suspend` + `WorkerSuspension`) and `harn-serve` A2A
adapter
([`crates/harn-serve/src/adapters/a2a/`][a2a-dir]).
**Sibling discussions:** [A2A #1857 — idempotency on
`tasks/send`][a2a-1857] covers a different concern (request
idempotency). A first-class paused state is still open.

[a2a]: https://github.com/a2aproject/A2A
[a2a-1857]: https://github.com/a2aproject/A2A/discussions/1857
[a2a-dir]: https://github.com/burin-labs/harn/tree/main/crates/harn-serve/src/adapters/a2a
[agents-rs]: https://github.com/burin-labs/harn/blob/main/crates/harn-vm/src/stdlib/agents.rs

## Problem statement

A2A's `TaskState` enum models task lifecycle as a state machine. The
current non-terminal "waiting" states are:

- `INPUT_REQUIRED` — the peer needs end-user input to continue.
- `AUTH_REQUIRED` — the peer needs the caller to refresh credentials
  or complete an auth flow.

Both are **callee-initiated soft-pauses** that exist to signal "I
literally cannot make progress until X is supplied." They name the
specific blocker so the caller's UI can prompt for it (a text input
prompt, an OAuth flow trigger).

A2A has no first-class state for **either**:

1. **`PAUSED_BY_CLIENT`** — the caller asked the peer to pause. The
   peer is fine; it just shouldn't make any further turns until told
   to.
2. **`PAUSED_BY_AGENT`** — the peer voluntarily parked itself waiting
   on an external condition (a file change, a CI build completion, a
   scheduled wake-up time) that's neither user input nor an auth
   refresh.

These are different shapes. Today A2A peers conflate them with
`INPUT_REQUIRED` (with a synthetic prompt the user is supposed to
ignore), `AUTH_REQUIRED` (definitely wrong), or `WORKING` (the
caller-side cancel button still nukes the task). All three workarounds
lose information: the caller's UI can't distinguish "paused, will
resume on its own" from "blocked, needs your input."

### Why this matters in practice

Concrete scenarios we hit shipping Harn:

- **Caller-driven pause for review.** A coordinator agent wants to
  pause a delegated worker, inspect its progress, then decide whether
  to resume or cancel. The coordinator needs to call `tasks/pause`
  and observe `PAUSED_BY_CLIENT` rather than send `INPUT_REQUIRED`
  back to itself.
- **Agent self-park on long waitpoints.** A peer agent calls a tool
  that spawns a CI build. The agent has nothing useful to do for
  minutes (possibly hours). Today it has to either burn idle turns
  polling or set `INPUT_REQUIRED` with a fake "waiting…" prompt; the
  caller's UI then has to know not to render it as a user-prompt.
- **Scheduled work.** "Pause until 09:00 UTC and continue." The peer
  knows the exact wake-up time; the caller doesn't need to be
  involved beyond observing the paused state.
- **Cost / budget interrupts.** A policy engine wants to pause every
  task that exceeds a token budget. The right state is
  `PAUSED_BY_CLIENT` (with a reason); the caller can decide whether
  to refill and resume or cancel.
- **Cross-protocol bridges.** Harn's `harn-serve` adapter today maps
  ACP `session/resume` (#1726) and Harn's `__host_worker_suspend`
  envelope onto A2A. With no `PAUSED` shape, the adapter has to
  invent its own `metadata.harn.paused` mapping; an external A2A
  client speaking to a Harn-backed peer can't observe the pause in
  any protocol-native way.

Harn ships all of this today through `__host_worker_suspend` (caller-
initiated) and `agent_await_resumption` (agent-initiated self-park),
built on a shared `WorkerSuspension` envelope. Both verbs are
currently tunneled through host-private metadata under
`metadata.harn.suspend`; the spec gap is the only thing preventing
external A2A clients from observing the pause natively.

### Why not extend `INPUT_REQUIRED`?

`INPUT_REQUIRED` is semantically "I am stopped because I lack a piece
of information the user has." Stretching it to mean "I am stopped
because the caller said so" or "I am stopped waiting on a deadline"
breaks the existing client contract:

- Client UIs render `INPUT_REQUIRED` as a prompt for user input. A
  user who sees that prompt for a caller-paused or self-parked task
  has no useful action to take.
- Resume callers MUST send a `Message` to flip out of
  `INPUT_REQUIRED`; we want to flip out of `PAUSED` with a verb
  (`tasks/resume`) that doesn't pollute the message stream.
- `INPUT_REQUIRED` is a single state; we need to distinguish
  caller-initiated from agent-initiated pauses for UI and audit.

## Proposed wire format

### `TaskState` additions

Two new non-terminal states, sibling to `INPUT_REQUIRED` /
`AUTH_REQUIRED`:

```typescript
export enum TaskState {
  // ...existing states...
  SUBMITTED = "submitted",
  WORKING = "working",
  INPUT_REQUIRED = "input-required",
  AUTH_REQUIRED = "auth-required",
  COMPLETED = "completed",
  CANCELED = "canceled",
  FAILED = "failed",
  REJECTED = "rejected",

  /**
   * Caller asked the peer to pause via `tasks/pause`. Peer commits
   * no further turns until `tasks/resume` is called or the task is
   * canceled.
   */
  PAUSED_BY_CLIENT = "paused-by-client",

  /**
   * Peer voluntarily parked itself waiting on an external condition
   * declared via `tasks/await_resumption`. Peer resumes when the
   * condition fires, the timeout elapses, or `tasks/resume` is
   * called explicitly.
   */
  PAUSED_BY_AGENT = "paused-by-agent",
}
```

### Task state machine deltas

Allowed transitions (additions only, existing transitions unchanged):

- `WORKING` → `PAUSED_BY_CLIENT` (via `tasks/pause`)
- `WORKING` → `PAUSED_BY_AGENT` (via `tasks/await_resumption`)
- `PAUSED_BY_CLIENT` → `WORKING` (via `tasks/resume`)
- `PAUSED_BY_AGENT` → `WORKING` (via `tasks/resume`, or when the
  agent's declared resume condition fires)
- `PAUSED_BY_CLIENT` → `CANCELED` (via `tasks/cancel`)
- `PAUSED_BY_AGENT` → `CANCELED` (via `tasks/cancel`)
- `PAUSED_BY_*` → `FAILED` (timeout elapsed with `on_timeout: "fail"`)

Notably **disallowed**: `INPUT_REQUIRED` ↔ `PAUSED_BY_*` direct
transitions. A peer that needs user input while paused must first
flip to `WORKING` and then to `INPUT_REQUIRED`; the two state
families don't compose because they have different unblock channels.

### `tasks/pause` (client → peer)

```json
{
  "jsonrpc": "2.0",
  "id": "req-019abf6b-...",
  "method": "tasks/pause",
  "params": {
    "taskId": "task-019abf6b-7d51-7c1d-bb02-...",
    "reason": "operator review",
    "mode": "finish_step",
    "metadata": {}
  }
}
```

Response:

```json
{
  "jsonrpc": "2.0",
  "id": "req-019abf6b-...",
  "result": {
    "taskId": "task-019abf6b-7d51-7c1d-bb02-...",
    "state": "paused-by-client",
    "handle": "suspend-019abf6b-...",
    "pausedAt": "2026-04-30T12:34:56.789Z",
    "reason": "operator review"
  }
}
```

`mode` mirrors the
`interrupt_immediate` / `finish_step` / `wait_for_completion`
taxonomy already discussed in our [sibling ACP
RFC](./acp-session-suspend.md). Defaults to `finish_step`.

### `tasks/await_resumption` (peer → client)

The agent-initiated dual. Lets a peer declare "I have nothing useful
to do until X" without round-tripping through the caller:

```json
{
  "jsonrpc": "2.0",
  "id": "req-019abf6b-...",
  "method": "tasks/await_resumption",
  "params": {
    "taskId": "task-019abf6b-7d51-7c1d-bb02-...",
    "reason": "waiting on ci/build:1234",
    "conditions": {
      "onEvent": "ci.build.completed:1234",
      "timeout": {
        "durationMinutes": 30,
        "onTimeout": "fail"
      }
    },
    "summary": "Paused on CI build 1234; ETA 3m.",
    "metadata": {}
  }
}
```

Response:

```json
{
  "jsonrpc": "2.0",
  "id": "req-019abf6b-...",
  "result": {
    "taskId": "task-019abf6b-7d51-7c1d-bb02-...",
    "state": "paused-by-agent",
    "handle": "suspend-019abf6b-...",
    "pausedAt": "2026-04-30T12:34:56.789Z"
  }
}
```

### `tasks/resume` (client → peer)

```json
{
  "jsonrpc": "2.0",
  "id": "req-019abf6b-...",
  "method": "tasks/resume",
  "params": {
    "taskId": "task-019abf6b-7d51-7c1d-bb02-...",
    "handle": "suspend-019abf6b-...",
    "input": null,
    "continueTranscript": true,
    "metadata": {}
  }
}
```

`input` is the optional value fed back to the peer's resume
waitpoint; `continueTranscript` controls whether the resumed turn
sees the full pre-pause transcript (default `true`) or a fresh turn
with a pre-pause digest (`false`). Both fields mirror the ACP
[`session/resume`](./acp-session-suspend.md) enrichment we propose
for symmetry.

### Streaming notifications

`SubscribeToTask` (already non-terminal-reconnect-safe) gains two
state-update notifications:

```json
{
  "jsonrpc": "2.0",
  "method": "tasks/statusUpdate",
  "params": {
    "taskId": "task-019abf6b-7d51-7c1d-bb02-...",
    "state": "paused-by-client",
    "handle": "suspend-019abf6b-...",
    "reason": "operator review",
    "initiator": "client",
    "pausedAt": "2026-04-30T12:34:56.789Z",
    "conditions": null
  }
}
```

and the symmetric resumed shape:

```json
{
  "jsonrpc": "2.0",
  "method": "tasks/statusUpdate",
  "params": {
    "taskId": "task-019abf6b-7d51-7c1d-bb02-...",
    "state": "working",
    "previousState": "paused-by-agent",
    "cause": "condition_fired",
    "hadResumeInput": false,
    "continueTranscript": true,
    "resumedAt": "2026-04-30T12:38:01.012Z"
  }
}
```

`cause` mirrors the ACP `SessionUpdate::Resumed.cause` enum verbatim
(`explicit_resume` / `condition_fired` / `timeout` / `external_event`)
so cross-protocol bridges round-trip causes byte-for-byte.

### Agent card capability

Peers that implement pause/resume advertise it on their agent card:

```json
{
  "name": "rebase-worker",
  "url": "https://example.com/.well-known/a2a-agent",
  "skills": ["rebase"],
  "capabilities": {
    "supportsPause": true,
    "supportsAwaitResumption": true,
    "resumeCauses": ["explicit_resume", "condition_fired", "timeout"]
  }
}
```

Callers MUST treat absent `capabilities.supportsPause` as "not
supported" and fall back to `tasks/cancel` (with the documented
caveat that the work is lost) or close the subscription and reconnect
to the persisted task without pausing.

## Error envelope

Errors follow A2A's existing JSON-RPC error envelope:

| Code | Meaning |
|---|---|
| `-32602` | Malformed `params` (missing `taskId`, unknown enum value on `mode` / `onTimeout`, etc.). |
| `-32004` | Unknown `taskId`. |
| `-32011` | Task is in a state that does not allow pause (e.g. already terminal). |
| `-32012` | Resume `handle` does not match the recorded suspension handle. |
| `-32601` | Peer does not implement `tasks/pause` (i.e., capability missing). |

## Compatibility and migration

### From the current `_meta`-shaped envelope

Harn-as-A2A-peer currently:

- Accepts caller-initiated pauses tunneled through `tasks/send`'s
  `metadata` map under `metadata.harn.pause.*`.
- Reports paused state on `tasks/statusUpdate` SSE events by leaving
  the wire state as `WORKING` (since A2A has no `PAUSED`) and
  decorating with `metadata.harn.pause` carrying the actual paused
  status, handle, reason, and resume conditions.
- Maps Harn's `WorkerSuspension` envelope (verbatim from
  [`crates/harn-vm/src/stdlib/agents.rs`][agents-rs]) onto the
  `metadata.harn.pause` shape.

Migration when the standardized state lands:

1. Promote paused state from `metadata.harn.pause.state` to top-level
   `TaskState.PAUSED_BY_CLIENT` / `PAUSED_BY_AGENT` on
   `tasks/statusUpdate` events.
2. Implement `tasks/pause`, `tasks/await_resumption`, and
   `tasks/resume` as canonical inbound paths. Keep
   `metadata.harn.pause` reads as a fall-back for one A2A minor
   version.
3. Add `capabilities.supportsPause` /
   `capabilities.supportsAwaitResumption` to the published agent
   card.
4. Regenerate `spec/protocol-artifacts/`
   (`make gen-protocol-artifacts`).

### For other A2A peers adopting this proposal

Peers that don't model pause internally can satisfy `tasks/pause` by
cancelling any in-flight tool calls (or letting them complete in
`wait_for_completion` mode), persisting the task's last known
state pointer, and returning a `handle` they can re-open on
`tasks/resume`. That's strictly stronger than the
`INPUT_REQUIRED`-with-fake-prompt workaround and requires no message
schema work. Implementing `tasks/await_resumption` is optional and
only needed by peers that want to self-park.

## Reference implementation status

| Surface | Status | Notes |
|---|---|---|
| `__host_worker_suspend` Rust builtin | Shipping (v0.8.x) | `crates/harn-vm/src/stdlib/agents.rs` — cooperative suspend at the next turn boundary; backs both caller- and agent-initiated paths. |
| `agent_await_resumption` script builtin | Shipping (v0.8.x) | `crates/harn-stdlib/src/stdlib/agent/workers.harn` — exposes the agent-initiated dual. |
| `WorkerSuspension` JSON envelope | Shipping | Shared verbatim with the [ACP RFC](./acp-session-suspend.md). |
| `ResumeConditions` validator (`parse_resume_conditions`) | Shipping | Validates `trigger` / `timeout` / `on_event` shape; backs the proposed `conditions` field field-for-field. |
| Suspend/resume conformance suite (S-11, #1847) | Shipping | Seven paired `.harn` / `.expected` fixtures cover caller suspend, agent self-park, timeout, double-resume race, close-while-suspended. |
| `InterruptAndSuspend` trigger handler (CH-10, #1910) | Shipping | Org-scoped panic broadcast that pause-bombs every running worker in a scope. Backs the cost / budget interrupt use case. |
| Lifecycle replay determinism receipts (P-08, #1861) | Shipping | `SuspensionReceipt` / `ResumptionReceipt` with HMAC-signed timestamps round-trip across record/replay. |
| OTel `Suspension` / `Resume` span pairing (S-18, #1867) | Shipping | Suspend span closes before snapshot persists; resume span links back to suspend + pipeline span at suspend time. |
| A2A adapter `metadata.harn.pause` outbound emission | Reference impl tracked under harn#1848 | Will emit under `metadata.harn.pause` until upstream lands. |
| Agent card `capabilities.supportsPause` advertisement | Pending upstream schema | Currently advertised under `capabilities._meta.harn.pause` (alongside `capabilities._meta.harn.reminders` from the [reminders RFC](./a2a-message-kind-reminder.md)). |

The canonical lifecycle struct ([`WorkerSuspension`][agents-rs]) is
shared verbatim with the [ACP RFC](./acp-session-suspend.md); field
names round-trip through the A2A JSON shape with conventional
camelCase translation.

## Open questions for upstream maintainers

1. **Two states vs one + initiator field.** We propose
   `PAUSED_BY_CLIENT` and `PAUSED_BY_AGENT` as separate states for
   the same reason we proposed `tasks/pause` and
   `tasks/await_resumption` as separate methods: the unblock channels
   differ (caller call vs condition / timeout / explicit resume) and
   client UIs render them differently. Maintainers may prefer a
   single `PAUSED` state plus an `initiator` discriminator on the
   status notification; we'd accept either, but the typed shape
   simplifies state-machine validators.
2. **Naming.** `PAUSED_BY_*` is verbose but unambiguous. Alternatives
   include `SUSPENDED_BY_CLIENT` (matches the ACP `session/suspend`
   verb), `STOPPED_BY_*` (overloaded with cancellation in some
   client UIs), or just `PAUSED` + `initiator` field. We've used
   `PAUSED_BY_*` to match Temporal's existing `WORKFLOW_PAUSED` /
   `WORKFLOW_PAUSED_BY_*` taxonomy.
3. **`mode` semantics.** Should `tasks/pause` honor the same
   `interrupt_immediate` / `finish_step` / `wait_for_completion`
   delivery modes as the ACP sibling? Our reference impl defaults to
   `finish_step` and exposes `interrupt_immediate` for the
   panic-broadcast `InterruptAndSuspend` trigger variant (#1910).
4. **`conditions` shape.** We propose three fields (`onEvent`,
   `trigger`, `timeout`). A2A maintainers may prefer a single opaque
   `Conditions` value the peer is free to parse, leaving the schema
   to peer extension. We've found the typed shape essential for
   replay determinism — peers that round-trip a condition need a
   stable schema for hashing.
5. **`continueTranscript` semantics.** Defaulting to `true` preserves
   the existing assumption that resumed tasks pick up where they left
   off with full transcript visibility. Defaulting to `false`
   matches the "fresh turn with a digest" pattern most production
   agents want. We've defaulted to `true` to match the ACP sibling.
6. **Push notification interaction.** A2A push notifications already
   exist; should `tasks/statusUpdate` with the new states piggyback
   on them or stay on the SSE stream? Our reference impl uses SSE
   only — push payloads weren't designed for the back-and-forth
   pause/resume conversation.
7. **Capability granularity.** Is `capabilities.supportsPause` /
   `supportsAwaitResumption` the right shape, or should they fold
   into an existing substructure? We've used the flat form for
   symmetry with the existing top-level capability flags.
8. **Relationship to the ACP RFC.** We've filed a parallel [ACP
   RFC](./acp-session-suspend.md) for `session/suspend` /
   `session/await_resumption`. The two RFCs deliberately share
   field names (`handle`, `reason`, `conditions`, `cause`) so
   cross-protocol bridges round-trip verbatim. If A2A's shape
   diverges substantially from ACP's, the cross-protocol story gets
   noisier.

## References

- [A2A #1857 — `tasks/send` idempotency][a2a-1857] (separate concern;
  not a substitute for explicit paused state)
- [Sibling ACP RFC: `session/suspend`](./acp-session-suspend.md)
- [Sibling A2A RFC: `tasks/inject_reminder`](./a2a-message-kind-reminder.md)
- [`__host_worker_suspend` builtin][agents-rs]
- [Harn A2A adapter][a2a-dir]
