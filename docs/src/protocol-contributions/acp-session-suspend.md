# ACP RFC: `session/suspend` + `session/await_resumption`

**Upstream repo:** [agentclientprotocol/agent-client-protocol][acp]
**Sibling discussions:** [ACP #1220 - `session/inject`][acp-1220],
[ACP #1224 - `session/remind`][acp-1224] (covered by the [sibling
`session/inject_reminder` RFC](./acp-session-inject-reminder.md));
this RFC is the suspend-side companion to the already-shipped
`session/resume` ([ACP #1726][acp-1726]).
**Discussion:** [ACP #1233 - `session/suspend` +
`session/await_resumption`][acp-1233].
**Status:** Open upstream discussion; no maintainer reply was present
when rechecked on 2026-06-27.
**Authors:** Burin Labs
**Reference impl:** `harn-vm` cooperative suspend primitive
([`crates/harn-vm/src/stdlib/agents.rs`][agents-rs] -
`__host_worker_suspend`; [`agents_workers/mod.rs`][workers-rs] -
`WorkerSuspension`) and `harn-serve` ACP adapter
([`crates/harn-serve/src/adapters/acp/mod.rs`][acp-mod-rs] -
`handle_session_resume`).

[acp]: https://github.com/agentclientprotocol/agent-client-protocol
[acp-1220]: https://github.com/agentclientprotocol/agent-client-protocol/discussions/1220
[acp-1224]: https://github.com/agentclientprotocol/agent-client-protocol/discussions/1224
[acp-1233]: https://github.com/agentclientprotocol/agent-client-protocol/discussions/1233
[acp-1726]: https://github.com/agentclientprotocol/agent-client-protocol/discussions/1726
[agents-rs]: https://github.com/burin-labs/harn/blob/main/crates/harn-vm/src/stdlib/agents.rs
[workers-rs]: https://github.com/burin-labs/harn/blob/main/crates/harn-vm/src/stdlib/agents_workers/mod.rs
[acp-mod-rs]: https://github.com/burin-labs/harn/blob/main/crates/harn-serve/src/adapters/acp/mod.rs

## Problem statement

ACP today has three session lifecycle verbs:

- `session/new` — open a fresh session.
- `session/load` — cold-replay a persisted session, re-emitting every
  historical `session/update` notification so the client can rebuild
  its view.
- `session/resume` (shipped in [ACP #1726][acp-1726]) — silent state
  restore: the agent picks up where it left off without replaying any
  history.

There is no companion **`session/suspend`** verb. A client that wants
to pause an in-flight session today has three workarounds, all bad:

1. **`session/cancel`** — tears the turn down, leaves no resumable
   handle, drops scheduled tool calls. Wrong semantics: cancellation
   is "I no longer want this work," not "pause and come back."
2. **Close the transport and reopen later** — works for `session/load`
   cold replay, but loses the warm in-process worker (provider
   sessions, prompt cache, attached tool handles). Forces the agent
   to redo work the spec explicitly added `session/resume` to avoid.
3. **Out-of-band signalling** — send a synthetic user message ("STOP")
   and hope the model emits no further tool calls. Non-deterministic
   and pollutes the durable transcript with control strings.

There is also no first-class verb for **agent-initiated self-park**.
The closest existing verb is `session/request_permission` (the agent
blocks until the host approves a specific action), but that's
permission-shaped — it carries a `RequestPermissionParams` payload,
the host must respond with allow/deny, and the session resumes
immediately on either answer. Agents that want to say "I'm done with
this turn, wake me when condition X holds (a file changes, a build
finishes, a deadline arrives)" have no shape for it.

The asymmetry is striking: ACP has `session/resume` but no
`session/suspend`; the agent can be cold-replayed via `session/load`
but cannot proactively park itself; mid-turn user steering exists
(`session/inject` from #1220, `session/remind` from #1224) but
mid-turn caller-initiated pauses do not.

### Why this matters in practice

Concrete scenarios we hit shipping Harn:

- **Long-running tool waitpoints.** A Harn agent calls a tool that
  spawns a long CI job. The agent wants to suspend until the build
  finishes (could be minutes to hours), without holding the model's
  prompt cache open or burning autonomy budget on idle turns.
- **Caller-driven pause for review.** A coordinator wants to pause a
  delegated worker, inspect its transcript, decide whether to let it
  continue or hand off to another worker. Today the coordinator has
  to invent its own out-of-band pause channel.
- **Scheduled work.** "Wake me at 09:00 UTC and continue" or "wake me
  when this channel emits an event." The agent describes the
  resumption condition once and parks; the host or runtime fires the
  resume when the condition holds.
- **Budget/cost interrupts.** A policy engine wants to pause every
  agent that exceeds a token budget until an operator decides whether
  to refill.

Harn ships all four today through `__host_worker_suspend` (caller-
initiated) and `agent_await_resumption` (agent-initiated self-park),
both built on the same `WorkerSuspension` envelope. Both verbs are
currently tunneled through host-private JSON-RPC under
`_meta.harn.suspend`; the spec gap is the only thing preventing
external ACP clients from speaking the same primitive.

## Proposed wire format

### `session/suspend` (client → agent)

A new JSON-RPC method on the agent side, sibling to `session/resume`
shipped in #1726:

```typescript
/**
 * Cooperatively pause an in-flight session at the next turn boundary.
 *
 * Suspend is NOT cancellation — the session remains valid and a
 * subsequent `session/resume` reopens it warm (or `session/load`
 * cold-replays it). Pending tool calls that have already been
 * dispatched complete; the agent commits to no further turns until
 * resumed.
 *
 * Mirrors mode semantics from `session/inject` (ACP #1220): the
 * default is to commit at the next turn boundary; `interrupt_immediate`
 * is a stronger guarantee for callers that need to halt mid-turn.
 */
export interface SessionSuspendRequest {
  /** Target session. */
  sessionId: SessionId
  /** Free-form reason recorded on the persisted suspension envelope. */
  reason?: string
  /**
   * Optional resume conditions the agent should satisfy before being
   * woken. When omitted, only explicit `session/resume` can wake the
   * session.
   */
  resumeWhen?: ResumeConditions
  /**
   * Delivery mode. Mirrors `session/inject` semantics.
   * - "finish_step" (default): commit at the next turn boundary.
   * - "interrupt_immediate": suspend before the next tool call dispatches.
   * - "wait_for_completion": let the in-flight turn fully resolve.
   */
  mode?: "interrupt_immediate" | "finish_step" | "wait_for_completion"
  /** Extension envelope for protocol-extension fields. */
  _meta?: Record<string, unknown>
}

export interface SessionSuspendResponse {
  /** Server-assigned handle the client must echo on `session/resume`. */
  handle: string
  /** Why the session is parked. */
  reason?: string
  /** Wall-clock time the suspension was committed (ISO-8601). */
  suspendedAt: string
  /** Resume conditions the agent will honor (echoed back to confirm). */
  resumeWhen?: ResumeConditions
  /**
   * Optional digest the agent recommends the client surface to the user
   * (e.g. "paused on long-running build; ETA 3m"). Plain prose, no
   * required structure.
   */
  summary?: string
  _meta?: Record<string, unknown>
}

export interface ResumeConditions {
  /** Resume on a named event (e.g. "ci.passed", "file.changed:src/lib.rs"). */
  onEvent?: string
  /** Resume on a trigger-spec match (host-defined event matcher). */
  trigger?: Record<string, unknown>
  /** Resume after a deadline elapses; action describes the fallback. */
  timeout?: ResumeTimeoutSpec
}

export interface ResumeTimeoutSpec {
  /** Wake-up deadline in minutes from suspend commit. */
  durationMinutes: number
  /** What to do on timeout if the explicit conditions never fire. */
  onTimeout?: "resume_with_summary" | "fail" | "resume_with_input"
}
```

### `session/await_resumption` (agent → client)

An agent-initiated dual to `session/suspend`. Lets the agent declare
"I have nothing more to do until X" without the client having to know
when to call `session/suspend`:

```typescript
/**
 * Agent-initiated self-park. The agent commits no further turns
 * until the supplied conditions are met or the client explicitly
 * resumes via `session/resume`.
 *
 * Distinct from `session/request_permission`, which is a synchronous
 * allow/deny gate for a specific action. await_resumption is
 * asynchronous and condition-driven.
 */
export interface SessionAwaitResumptionRequest {
  sessionId: SessionId
  /** Free-form reason. */
  reason: string
  /** Conditions for the host to satisfy before resuming. */
  conditions?: ResumeConditions
  /**
   * Optional summary the client should surface in its UI while the
   * agent is parked.
   */
  summary?: string
  _meta?: Record<string, unknown>
}

export interface SessionAwaitResumptionResponse {
  /** Same handle shape as session/suspend; same resume entrypoint. */
  handle: string
  suspendedAt: string
  _meta?: Record<string, unknown>
}
```

### Resume path enrichment

`session/resume` (already shipped in #1726) gains two optional fields
without breaking existing callers:

```typescript
export interface SessionResumeRequest {
  /** Existing field from #1726. */
  sessionId: SessionId
  /**
   * Optional value to feed back to the agent's resume waitpoint.
   * Mirrors `resume_with_input` semantics in our reference impl.
   */
  input?: unknown
  /**
   * When true (default), the resumed turn sees the full pre-suspend
   * transcript. When false, the agent starts a fresh turn with a
   * pre-suspend digest reminder injected (mirrors our
   * `continue_transcript: false` mode).
   */
  continueTranscript?: boolean
  _meta?: Record<string, unknown>
}
```

### Lifecycle notification: `session/update` → `suspended` / `resumed`

Two new discriminators on the existing `session/update` union so
observers (notebooks, transcript browsers) can render suspended state
without polling:

```typescript
export interface SessionUpdate_Suspended {
  sessionUpdate: "suspended"
  handle: string
  reason?: string
  initiator: "client" | "agent"
  suspendedAt: string
  resumeWhen?: ResumeConditions
  _meta?: Record<string, unknown>
}

export interface SessionUpdate_Resumed {
  sessionUpdate: "resumed"
  handle: string
  /** Why the agent was woken. */
  cause: "explicit_resume" | "condition_fired" | "timeout" | "external_event"
  /** True iff a value was fed back via `input`. */
  hadResumeInput: boolean
  /** Mirrors the `continueTranscript` field on the resume request. */
  continueTranscript: boolean
  resumedAt: string
  _meta?: Record<string, unknown>
}
```

### Capability negotiation

Agents that support suspend declare it during `initialize`:

```typescript
export interface AgentCapabilities {
  // ...existing fields...
  /** Agent accepts `session/suspend`. */
  supportsSuspend?: boolean
  /** Agent emits `session/await_resumption`. */
  supportsAwaitResumption?: boolean
  /** Resume modes the agent can honor. Absent means [\"explicit_resume\"]. */
  resumeCauses?: ("explicit_resume" | "condition_fired" | "timeout" | "external_event")[]
}
```

Clients that observe an agent without `supportsSuspend` should fall
back to `session/cancel` (with the documented caveat that the work is
lost) or close the transport and use `session/load` for a cold replay
on the next connection.

## Compatibility and migration

### From the current `_meta.harn.suspend` envelope

Harn ships today with:

- `__host_worker_suspend(worker, reason, options?)` builtin (see
  [`crates/harn-vm/src/stdlib/agents.rs`][agents-rs]) — the runtime-
  level cooperative suspend primitive.
- `agent_await_resumption(reason, conditions?)` script-facing builtin
  for agent-initiated self-park.
- `WorkerSuspension` JSON envelope persisted to `.harn-runs/` and
  exchanged across the host boundary under `_meta.harn.suspend.*` on
  the existing `session/update` notification surface.
- `session/resume` ACP method (already standardized via #1726, handled
  by `handle_session_resume` in
  [`crates/harn-serve/src/adapters/acp/mod.rs`][acp-mod-rs]).

Migration steps once the upstream schema lands:

1. Add `session/suspend` and `session/await_resumption` as siblings of
   the existing `session/resume` handler in `harn-serve`; mark the
   `_meta.harn.suspend` JSON-RPC tunnel deprecated in the `_meta`
   extension contract.
2. Promote `WorkerSuspension` envelope fields (`handle`, `reason`,
   `resume_when`, `suspended_at`) from `_meta.harn.suspend.*` to
   top-level standardized fields on `SessionSuspendResponse`.
3. Emit `SessionUpdate::Suspended` and `SessionUpdate::Resumed` from
   the existing lifecycle event sinks; drop `_meta.harn.suspend`
   decoration on `session/update` notifications once consumers
   migrate.
4. Regenerate `spec/protocol-artifacts/` via
   `make gen-protocol-artifacts`.

No-op for older clients: `session/cancel`-as-fallback continues to
work, and the `_meta.harn.suspend` envelope stays available for at
least one ACP minor version after the standardized fields ship.

### For other ACP agents adopting this proposal

Agents that don't currently have a cooperative suspend primitive can
implement `session/suspend` as a thin wrapper that:

1. Cancels any in-flight tool calls (or lets them complete in
   `wait_for_completion` mode).
2. Persists the session's last known prompt + transcript pointer.
3. Returns a handle the agent can re-open on `session/resume`.

That's strictly stronger than the `session/cancel`-and-cold-replay
workaround and requires no transcript schema work. Adding
`session/await_resumption` is optional and only needed by agents that
want to self-park.

## Reference implementation status

| Surface | Status | Notes |
|---|---|---|
| `__host_worker_suspend` Rust builtin | Shipping (v0.8.x) | `crates/harn-vm/src/stdlib/agents.rs` — cooperative suspend at the next turn boundary. |
| `agent_await_resumption` script builtin | Shipping (v0.8.x) | `crates/harn-stdlib/src/stdlib/agent/workers.harn` — exposes the agent-initiated dual. |
| `WorkerSuspension` JSON envelope | Shipping | Round-trips suspend handle, reason, resume conditions, pipeline + parent worker linkage. |
| `ResumeConditions` validator (`parse_resume_conditions`) | Shipping | Validates trigger / timeout / on_event shape; backs the proposed `ResumeConditions` schema field-for-field. |
| `session/resume` ACP method | Shipping (via [ACP #1726][acp-1726]) | `handle_session_resume` in `crates/harn-serve/src/adapters/acp/mod.rs`. |
| `session/suspend` ACP method | Reference impl tracked under this issue (harn#1848) | Will tunnel under `_meta.harn.suspend` until upstream lands. |
| `session/await_resumption` ACP method | Reference impl tracked under this issue | Same `_meta.harn.suspend` tunnel; agent-initiated dispatch. |
| `SessionUpdate::Suspended` / `Resumed` wire shape | Reference impl tracked under this issue | Will emit under `_meta.harn.suspend` until upstream lands. |
| Suspend/resume conformance suite (S-11, #1847) | Shipping | Seven paired `.harn` / `.expected` fixtures under `conformance/tests/agents/` covering caller suspend, agent self-park, timeout, double-resume race, close-while-suspended. |
| OTel `Suspension` / `Resume` span pairing (S-18, #1867) | Shipping | Suspend span closes before snapshot persists; resume span links (not parents) back to suspend + pipeline span at suspend time. |
| Lifecycle replay determinism receipts (P-08, #1861) | Shipping | `SuspensionReceipt`, `ResumptionReceipt`, `DrainDecisionReceipt` with HMAC-signed timestamps round-trip across record/replay. |

The canonical `WorkerSuspension` lifecycle envelope (verbatim from
`crates/harn-vm/src/stdlib/agents_workers/mod.rs`) maps field-for-
field onto the proposed wire format:

```rust
pub struct WorkerSuspension {
    pub handle: String,
    pub reason: Option<String>,
    pub resume_when: Option<ResumeConditions>,
    pub initiator: SuspensionInitiator,   // Client | Agent
    pub suspended_at_ms: i64,
    pub pipeline_span_link: Option<SpanLink>,
    pub prior_span_link: Option<SpanLink>,
    pub parent_worker_id: Option<String>,
}

pub struct ResumeConditions {
    pub on_event: Option<String>,
    pub trigger: Option<serde_json::Value>,
    pub timeout: Option<ResumeTimeoutSpec>,
}

pub struct ResumeTimeoutSpec {
    pub duration_minutes: i64,
    pub on_timeout: Option<ResumeTimeoutAction>, // resume_with_summary | fail | resume_with_input
}
```

## Open questions for upstream maintainers

1. **Naming.** `session/suspend` mirrors the existing
   `session/resume`. An alternative is `session/pause`, which avoids
   the connotation that suspend is a stronger / colder shape than
   resume's twin. We've used `suspend` to match the verb already in
   common use in Temporal, Restate, and Azure Durable Functions for
   the same primitive.
2. **Initiator discriminator.** We've split caller-initiated
   (`session/suspend`) from agent-initiated
   (`session/await_resumption`) into two methods because the request
   payloads differ enough (mode + sessionId vs reason + conditions)
   that overloading one method would force optional fields whose
   validity depends on the caller direction. Maintainers may prefer a
   single `session/suspend` with an `initiator` field; we'd accept
   either.
3. **`mode` semantics.** Should `session/suspend` honor the same
   `interrupt_immediate` / `finish_step` / `wait_for_completion`
   delivery modes as `session/inject`, or always behave as
   `finish_step`? Our reference impl defaults to `finish_step` (the
   safest shape) and exposes `interrupt_immediate` for the
   panic-broadcast `InterruptAndSuspend` trigger handler variant
   (#1910) used for cost / budget interrupts.
4. **`resumeWhen` shape.** We propose three fields (`onEvent`,
   `trigger`, `timeout`). Maintainers may prefer a single opaque
   `Conditions` value the agent is free to parse, leaving the schema
   to host extension. We've found the typed shape essential for
   replay determinism — agents that round-trip a condition need a
   stable schema for hashing.
5. **`continueTranscript` semantics.** Defaulting to `true` preserves
   `session/resume`'s current behavior (#1726 doesn't mention
   transcript scoping because cold-replay always replays the full
   thing). Defaulting to `false` matches the "fresh turn with a
   digest" pattern most production agents want. We've defaulted to
   `true` to preserve back-compat with #1726, but the right default
   for the spec long-term may be `false`.
6. **`SessionUpdate::Suspended` / `Resumed` granularity.** Are these
   in scope for the first iteration, or should the spec ship only the
   methods and leave lifecycle notifications for a follow-up? Our
   experience with reminder lifecycle (see the [sibling reminder
   RFC](./acp-session-inject-reminder.md)) says notebook / transcript
   UIs need at least suspend / resume visibility to render coherent
   timelines.
7. **Relationship to A2A `TaskState.PAUSED`.** We've filed a parallel
   [A2A RFC](./a2a-paused-state.md) for an explicit `PAUSED` task
   state distinct from `INPUT_REQUIRED` / `AUTH_REQUIRED`. The
   `cause` enum on `SessionUpdate::Resumed` deliberately mirrors the
   A2A pause-cause taxonomy so cross-protocol bridges (Harn's
   `harn-serve` adapter today) can round-trip causes verbatim. If
   ACP's verb shape diverges substantially from A2A's, the
   cross-protocol story gets noisier.

## References

- [ACP #1726 — `session/resume` (already shipped)][acp-1726]
- [ACP #1220 — `session/inject` (mode semantics borrowed)][acp-1220]
- [ACP #1224 — `session/remind` (sibling lifecycle RFC)][acp-1224]
- [Sibling A2A RFC: `TaskState.PAUSED`](./a2a-paused-state.md)
- [Sibling ACP RFC: `session/inject_reminder`](./acp-session-inject-reminder.md)
- [Harn ACP/MCP extensions v1](../spec/harn-extensions/v1.md)
- [`__host_worker_suspend` builtin][agents-rs]
- [`handle_session_resume` ACP handler][acp-mod-rs]
