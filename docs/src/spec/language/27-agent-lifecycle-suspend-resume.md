<!-- Generated from spec/chapters/*.md by scripts/sync_language_spec.harn -->

## Agent lifecycle (suspend/resume)

Agent workers are cooperatively schedulable. A running worker may yield —
mid-loop, at a turn boundary — to be resumed later in the same process,
a different process, or on another machine. The primitive is shared by
script-driven suspends, model-driven self-parks via the
`agent_await_resumption` lifecycle tool, and the daemon idle path.

User-facing reference: `docs/src/agent-lifecycle.md`. Upstream protocol
companions are documented at
`docs/src/protocol-contributions/acp-session-suspend.md` and
`docs/src/protocol-contributions/a2a-paused-state.md`.

### Lifecycle states

A worker observes a strict state machine:

```text
            spawn_agent
                |
                v
  +---------+ suspend_agent / agent_await_resumption +-----------+
  | running | -----------------------------------------> | suspended |
  +---------+                                            +-----------+
       ^                                                     |
       |             resume_agent / subagent_resume          |
       +-----------------------------------------------------+
       |                                                     |
       | natural completion                                  | close_agent
       v                                                     v
  +---------+                                            +-----------+
  |  done   |                                            |  closed   |
  +---------+                                            +-----------+
```

Transitions:

- `running -> suspended`: cooperative. `suspend_agent` flips the
  worker's suspend flag; the worker honors it at the next turn boundary
  (after the current LLM call and tool dispatch settle). A snapshot is
  persisted before the state flips. Idempotent: a second `suspend_agent`
  on an already-suspended worker returns the current summary unchanged
  and does not re-emit `WorkerSuspended`.
- `suspended -> running`: `resume_agent(handle_or_snapshot, input?,
  continue_transcript?)` rehydrates the worker. Concurrent resumes raise
  `HARN-SUS-006`; the second caller may retry against the now-running
  handle.
- `running -> done`: natural completion through `agent_loop` end
  conditions; emits `WorkerCompleted`.
- `suspended -> closed`: `close_agent` marks the snapshot rejected;
  later resume attempts raise `HARN-SUS-010`.

Invalid transitions raise specific diagnostics:

| From | Op | Diagnostic |
|---|---|---|
| `closed`, `done` | `suspend_agent` | `HARN-SUS-001` |
| `running` | `resume_agent` | `HARN-SUS-003` |
| `closed` (snapshot rejected) | `resume_agent` | `HARN-SUS-010` |

### `Suspension`

A `Suspension` is the canonical payload returned to the parent (or
direct caller) when a worker parks:

```text
Suspension = {
  status:                "suspended",            // discriminator
  handle:                WorkerHandle,           // resumable reference
  reason:                string,                 // operator / model-supplied
  initiator:             "self" | "parent" | "operator" | "triggered",
  conditions:            ResumeConditions | nil,
  iterations_completed:  int,                    // completed loop turns
  suspended_at:          string,                 // RFC3339 timestamp
  suspended_at_turn:     int | nil,              // loop turn index, when known
  snapshot_path:         string,                 // persisted snapshot
  resume_by_mechanism:   string | nil,           // e.g. "ResumeBy.parent_llm"
  auto_resume_trigger:   string | nil,           // registered trigger id
}
```

Top-level `agent_loop(...)` returns the same shape with `handle` set to
the persisted snapshot. The CLI cold-restores it with `harn run --resume
<snapshot_path>`. The runtime never persists transcript text, resume
input, or condition payloads in the `Suspension` envelope itself —
sensitive content stays in the worker snapshot and the transcript store.

### `ResumeConditions`

`ResumeConditions` declares what may wake a suspended worker. All three
fields are optional:

```text
ResumeConditions = {
  trigger?:  TriggerSpec,
  timeout?:  {duration_minutes: int, on_timeout?: TimeoutAction},
  on_event?: string,
}

TimeoutAction = "resume_with_summary" | "fail" | "resume_with_input"
```

- `trigger` is validated by the same trigger-spec parser used by
  `trigger_register(...)`. Any provider that works as a trigger source
  (`github`, `slack`, `cron`, `channel`, `webhook`, etc.) works as a
  resume condition. Registration failures raise `HARN-SUS-007`.
- `timeout.duration_minutes` must be a positive integer.
  `timeout.on_timeout` defaults to `"resume_with_summary"`. Unsupported
  values raise `HARN-SUS-008`.
- `on_event` must be a non-empty EventLog topic name.

`parse_resume_conditions(nil)` returns `nil`. Invalid input raises
`HARN-SUS-002` with the failing field path. A worker with no conditions
is parked "open" — only the parent agent, an operator, or an explicit
`resume_agent(...)` call can wake it.

### `agent_await_resumption`

`agent_await_resumption(reason, conditions?, resume_by?)` returns a
normalized `AgentAwaitResumptionRequest`:

```text
AgentAwaitResumptionRequest = {
  reason:     string,                                   // trimmed, non-empty
  conditions: ResumeConditions | nil,
  resume_by:  (harness, suspension) -> dict | nil,
}
```

Empty or whitespace-only `reason` raises `HARN-SUS-005`. The function
itself is pure validation; the structural integration happens inside
`agent_loop(...)`:

- When called from inside an `agent_loop` running as a worker, the loop
  intercepts the request *before* normal tool dispatch, persists a
  snapshot, and returns the `Suspension` payload to the parent.
- When called from outside that structural integration (e.g. as a
  direct tool handler), it raises `HARN-SUS-005`.

### Resume responsibility (`ResumeBy.*`)

Every suspension has exactly one resume owner. The optional `resume_by`
argument is a callback with signature
`(harness, Suspension) -> {handled: bool, mechanism: string, reason?: string}`.

Four canonical presets live in `std/agent/resume_by`:

| Preset | Behavior |
|---|---|
| `ResumeBy.parent_llm` | Always returns `{handled: true, mechanism: "ResumeBy.parent_llm"}`. The parent agent's LLM owns the resume via `subagent_resume`. |
| `ResumeBy.local_runtime` | Registers `conditions.trigger` with the in-process trigger dispatcher. Declines with `{handled: false, reason: "no_conditions"}` when no conditions are present. |
| `ResumeBy.cloud_harness` | Registers the suspension with a cloud webhook receiver for durability across process restart. Declines with `{handled: false, reason: "no_cloud_session"}` when no cloud session is bound. |
| `ResumeBy.pipeline_drain` | Always returns `{handled: true, mechanism: "ResumeBy.pipeline_drain"}`. The enclosing pipeline's drain step owns the resume. |

Presets compose with `std/lifecycle/combinators::first_available`
(re-exported as `first_handled` in `std/agent/resume_by`). When the
script omits `resume_by`, the runtime calls `default_resume_by(...)`:

- `conditions == nil` → `ResumeBy.parent_llm`
- `conditions != nil`, no cloud session → `ResumeBy.local_runtime`
- `conditions != nil`, cloud session → `first_handled([cloud_harness, local_runtime])`

Every resolved dispatch emits a `resume_by_dispatched` audit through
`harness.emit_audit`. Declined entries emit `resume_by_declined`. A
callback that returns `{handled: false}` or throws falls back to
`ResumeBy.parent_llm` so the parent agent is always the safety net.
Audits surface through `lifecycle_audit_log_take()`.

### Transcript continuity

`resume_agent(worker_or_snapshot, input? = nil, continue_transcript? = true)`
controls how the resumed worker sees the prior loop:

- `continue_transcript: true` (default) — the full transcript is
  preserved. The runtime injects a single-shot `system_reminder` event
  with `dedupe_key: "resume_continuity"` summarizing the suspension
  reason, the gap, and the resume cause. The reminder is consumed on the
  first resumed turn and is *not* re-applied on subsequent suspends of
  the same worker.
- `continue_transcript: false` — the worker resumes from the prior
  transcript summary plus the new input only. Useful when the previous
  deliberation reached a dead-end and the next turn should think from
  first principles.

Resume input validation failures raise `HARN-SUS-009`.

### Lifecycle hooks

Suspend and resume fire structured hook events around the state
transition:

- `PreSuspend` (advisory; supports `Allow`/`Deny` decisions). A `Deny`
  cancels the suspend; the worker remains `running` and the operator
  receives `pre_suspend_denied` audit metadata.
- `PostSuspend` (advisory only; runs after the snapshot is persisted
  and the `WorkerSuspended` event is emitted).
- `PreResume` / `PostResume` (analogous; `PreResume` Deny cancels the
  resume and the worker remains `suspended`).

Hook payloads carry the structured suspend metadata (handle, reason,
initiator, condition presence flags) without exposing transcript text or
resume-input contents.

### Top-level loops

A root `agent_loop(...)` (one called from a pipeline or `harn run`
script, not as a child) uses the same suspend path. On self-park the
runtime persists a snapshot under `.harn/workers/worker_*.json` and
returns the `Suspension` shape to the caller. The CLI restores it in any
subsequent process:

```text
harn run --resume <snapshot_path>
```

`--resume` accepts absolute or script-relative paths. `--json` emits the
final value as a structured event when the resumed loop terminates.

### Daemon idle

`agent_loop(..., {daemon: true})` and the `daemon_*` stdlib wrappers
internally call `agent_await_resumption(...)` when no wake source
(messages, `agent/resume` notification, `wake_interval_ms`, watched
paths) is queued. The persisted snapshot extends the standard suspend
metadata with daemon-specific fields (`pending_event_count`,
`queued_event_count`, `inflight_event`, `wake_interval_ms`,
`watch_paths`, `event_queue_capacity`). `daemon_resume(path)` cold-
restores the loop identically.

### Cooperative cancellation contract

Suspend is cooperative, not preemptive. The runtime flips the worker's
suspend flag; the loop honors it only at a turn boundary (after the
current LLM call and tool dispatch settle). Workers that do not return
to a turn boundary — e.g. a tool that blocks forever inside a single
call — never observe the request. Bounded execution of long-running
tools is the caller's responsibility (`tool_call_timeout`,
`async_tool_run`, etc.).

This is the same cooperative model as `agent_loop`'s natural completion
gate: state transitions happen at boundaries the runtime owns, not
inside opaque calls the model issued.

