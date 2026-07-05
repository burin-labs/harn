# Pipeline lifecycle

> **Pipeline vs. workflow.** Two different things that are deliberately not
> renamed — learn the distinction once:
>
> - **`pipeline`** is the *language keyword*: a named, callable, function-like
>   composition (`pipeline name(args) { ... }`) that serves as a program
>   entrypoint and container. Not itself agentic. This page describes its
>   finish-time lifecycle.
> - **Workflow** is the *stage-graph runtime*: the typed, replayable graph of
>   stages executed by `workflow_execute`. See the
>   [workflow runtime](./workflow-runtime.md).
>
> On the abstraction ladder: `llm_call` = one request < `agent_loop` = one
> goal < workflow = multiple goals, attempts, or models. (`agent_preset` is
> how you build `agent_loop` options, not a tier of its own.) See
> [Choosing an agent abstraction](./concepts/abstraction-ladder.md) and the
> [glossary](./concepts/glossary.md).

Pipelines in Harn do not end the moment their declared steps return.
Between the last statement of the pipeline body and the value the host
sees, the runtime fires a sequence of lifecycle callbacks: `PreFinish`,
the user-registered `on_finish` (which can transform the return value),
`OnUnsettledDetected` (when work survives the body), and `PostFinish`.
The same callback shape — `fn(harness, return_value) -> return_value` —
threads through every lifecycle gate, so presets and combinators that
work for `on_finish` also work for hook handlers, `resume_by`
callbacks, and any custom drain logic.

This page is the long-form reference. For a copy-paste quickref see the
"Pipeline lifecycle" section in `docs/llm/harn-quickref.md`. For
end-to-end patterns see the [lifecycle cookbook](./cookbooks/lifecycle.md).
For the per-preset surface (`on_finish_drain`, `on_finish_abandon`,
combinators, `OnBudget`) see [Pipeline lifecycle presets](./stdlib/lifecycle.md).
For the suspend/resume primitive that drives `suspended_subagents` see
the agent-lifecycle entry in the quickref (harn#1836).

## Why callbacks instead of enum-dispatch

The pipeline DSL could have shipped an enum-shaped option:

```text
pipeline_finish_policy: drain | abandon | block_until_settled
```

Three reasons made the callback-first shape win:

1. **Composability.** Real pipelines want "wait briefly, otherwise hand
   off to a long-running pipeline, otherwise drain immediately." With
   an enum we would add `OneOf` and `Fallback` variants; with closures
   it is just function composition.
2. **Custom audit trails.** Production callers want to record
   per-disposition decisions in their own store, not just the
   built-in `pipeline.lifecycle.audit` topic. A callback can call
   `harness.emit_audit(...)` *and* push to an external store; an enum
   value cannot.
3. **Symmetry with hooks.** Every other lifecycle gate
   (`pre_finish`, `pre_suspend`, `on_drain_decision`, ...) is already a
   user-supplied closure. Reusing the closure shape for the
   pipeline-finish step keeps a single mental model — and lets the
   `std/lifecycle/combinators` factories wrap any of them.

Temporal's `wait_condition` and Restate's saga primitives use the same
shape for the same reasons; the design here is a deliberate borrow with
Harn-native combinator factories.

## Lifecycle event order

When a pipeline's declared steps complete, the runtime walks this
sequence on the main VM:

1. **`PreFinish`** — last chance to inject a reminder before the
   pipeline value is captured. Rejects `{block: true}`; use
   `OnFinish.block_until_settled` to gate finish on work draining.
2. **The registered `on_finish` callback** — receives
   `(harness, return_value)`. Its return value replaces the pipeline's
   return value. Default behavior (no registration) is identical to
   `on_finish_abandon`.
3. **`OnUnsettledDetected`** — fires after the callback if any bucket
   in `harness.unsettled_state()` is still non-empty. Accepts
   `{block: true, reason}` to delay finish until the host explicitly
   drains, and `{modify: payload}` to amend the snapshot.
4. **`PostFinish`** — advisory; observe the final value, push telemetry.
5. The value is returned to the host.

Each step records `hook_call` / `hook_returned` / `hook_vetoed` events
on the active session transcript so replay reproduces the same control
flow.

## The harness

The single argument to every lifecycle callback is the `harness`. It
exposes one read-side surface and a dozen write-side actions.

### Read: unsettled state

`harness.unsettled_state()` returns a stable dict with five lists:

| Bucket | Producer |
|---|---|
| `suspended_subagents` | `agent_await_resumption` and `spawn_agent({resume_when})` (harn#1836). |
| `queued_triggers` | Trigger inbox + worker queue event-log records. |
| `partial_handoffs` | `harness.handoff_to(target, payload)` envelopes. |
| `in_flight_llm_calls` | The live LLM call registry. |
| `pool_pending_tasks` | `pool.submit(...)` tasks (queued + running). |

`harness.is_empty(state?)`, `harness.counts(state?)`, and
`harness.summary(state?)` summarize either an already-captured snapshot
or a fresh one. Pass the snapshot you took into `is_empty` /
`counts` / `summary` to keep one decision consistent within a callback;
omit the argument to ask the harness for a new snapshot each call.

The `std/lifecycle` module re-exports the same shape as module-level
helpers: `unsettled_state(harness)`, `is_empty(state)`, `counts(state)`,
`summary(state)`.

### Write: drain actions

| Method | Effect |
|---|---|
| `harness.resume_subagent(handle, input?)` | Resume a suspended worker; falls back to send-input for awaiting retriggerables. |
| `harness.cancel_subagent(handle, reason?)` | Close a suspended worker. Returns the worker summary. |
| `harness.handoff_to(target_pipeline, payload?)` | Record a partial-handoff envelope. Returns `{status: "queued", envelope}`. |
| `harness.acknowledge_trigger(id)` | Settle one queued trigger inbox or worker-queue item. |
| `harness.defer_trigger(id, target_pipeline?)` | Ack the trigger and emit a partial-handoff envelope (default target `deferred-triggers`). |
| `harness.acknowledge_handoff(envelope_id, decision?)` | Remove a partial envelope and emit a `handoff_acknowledged` audit. |
| `harness.wait_for_any_settlement(max_duration?)` | Snapshot + return `{status, timed_out, state}`. |
| `harness.emit_audit(kind, payload?)` | Append a typed entry to the per-run audit log (and to `pipeline.lifecycle.audit` when an EventLog is installed). |
| `harness.finalize(disposition?)` | Stamp the run's final disposition + emit `pipeline_finalized`. |
| `harness.spawn_settlement_agent(unsettled, return_value)` | Hand off to the bounded settlement-agent drain loop (P-03, #1856). |
| `harness.current_pipeline_id()` | Run id when a mutation session is installed, otherwise session id, otherwise nil. |

Ordering enforcement: `acknowledge_trigger` and `acknowledge_handoff`
reject out-of-order calls with `HARN-DRN-001`. The bucket order is
suspended subagents → queued triggers → partial handoffs → in-flight
LLM calls → pool pending tasks. The settlement-agent loop respects the
same order automatically.

## `OnFinish` presets

Four canonical presets ship from `std/lifecycle`. Each is a pure
function (or a pure factory returning one) with no captured state, so
chaining and reuse are safe.

### `on_finish_abandon`

Reproduces today's no-callback behavior, but emits a
`pipeline_abandoned_unsettled` audit when unsettled state is non-empty
so the lost work is at least observable. Returns `return_value`
unchanged.

```harn
import { on_finish_abandon } from "std/lifecycle"

pipeline default() {
  pipeline_on_finish(on_finish_abandon)
  return "ok"
}
```

### `on_finish_drain`

The recommended default. Scans `harness.unsettled_state()` and either
finalizes immediately (when nothing is deferred) or delegates the
per-item disposition to `harness.spawn_settlement_agent`. The
settlement-agent loop walks the buckets in the canonical order,
applies a default disposition per item (cancel suspended subagents,
acknowledge stale triggers, defer partial handoffs, drain in-flight
LLM calls, defer pool tasks), and emits a `drain_decision` audit per
item. The loop is bounded by a per-call budget (default 5,
configurable up to 20); on exhaustion a `drain_unsettled_remaining`
audit captures the remainder.

```harn
import { on_finish_drain } from "std/lifecycle"

pipeline default() {
  pipeline_on_finish(on_finish_drain)
  return "triage complete"
}
```

### `on_finish_block_until_settled(timeout, fallback?)`

Returns a callback that polls `harness.wait_for_any_settlement` until
everything drains or the timeout elapses. On a clean settle it emits
`pipeline_finalized:settled_within_timeout` and returns the unchanged
value; on timeout it emits `settlement_timeout` and delegates to the
fallback (default `on_finish_drain`). Use this preset to make
`PreFinish`-level blocks unnecessary.

```harn
import { on_finish_block_until_settled } from "std/lifecycle"

pipeline default() {
  pipeline_on_finish(on_finish_block_until_settled(30s))
  return "ok"
}
```

### `on_finish_handoff_to(target_pipeline, options?)`

Returns a callback that packages the current unsettled-state snapshot
into a typed envelope (with `origin`, `unsettled`, and any caller
options) and hands it off via `harness.handoff_to`. When there is
nothing unsettled, the callback short-circuits to `pipeline_finalized`
and returns the unchanged value — the typical case where the handoff
pipeline does not need to run at all.

```harn
import { on_finish_handoff_to } from "std/lifecycle"

pipeline default() {
  pipeline_on_finish(on_finish_handoff_to("nightly-drain"))
  return "triage complete"
}
```

## Callback combinators

`std/lifecycle/combinators` exports six pure factories that wrap any
`(harness, return_value) -> return_value` callback — hook handlers,
`resume_by` callbacks, the presets above, and any custom `on_finish`
body. All six are pure factories with no captured mutable state, so
they nest freely.

| Combinator | Behavior |
|---|---|
| `compose([cb, ...])` | Runs each callback in order, threading the prior return value into the next callback's `return_value`. Returns the last entry's value. |
| `first_available([cb, ...])` | Invokes in order; returns the first non-nil result. |
| `with_telemetry(cb, span_name?)` | Wraps with a `SpanKind::FnCall` OTel span and emits paired `{span_name}_started` / `_completed` / `_errored` audit entries. |
| `with_timeout(cb, ms)` | Soft, clock-aware deadline. On overrun returns `{__timed_out: true, timeout_ms, elapsed_ms, return_value}` and emits `lifecycle_callback_timed_out`. |
| `if_unsettled(cb)` | Only invokes when `harness.unsettled_state()` is non-empty (one snapshot per call). |
| `when(predicate, cb)` | Only invokes when `predicate(harness, return_value)` is truthy; otherwise passes the inbound value through. |

A realistic production chain — "wait briefly, otherwise hand off to a
nightly settlement pipeline, otherwise drain immediately, with
telemetry around the whole thing" — reads top-to-bottom:

```harn
import {
  on_finish_block_until_settled,
  on_finish_drain,
  on_finish_handoff_to,
} from "std/lifecycle"
import { compose, if_unsettled, with_telemetry } from "std/lifecycle/combinators"

pipeline default() {
  pipeline_on_finish(
    with_telemetry(
      if_unsettled(
        on_finish_block_until_settled(
          30s,
          on_finish_handoff_to("nightly-drain"),
        ),
      ),
      "pipeline_finish",
    ),
  )
  return "ok"
}
```

## Budget-threshold callbacks

`std/lifecycle/on_budget` ships three named callback strategies for the
`OnBudgetThreshold` lifecycle event. Each follows the same
`(harness, budget_state) -> result` shape, so they compose freely with
the combinators above.

| Strategy | Behavior |
|---|---|
| `OnBudget.terminate` | Emits `budget_exceeded`; throws `{category: "budget_exceeded", kind: "terminal", reason: "on_budget_terminate", strategy: "terminate", budget_state, message}` so the enclosing loop unwinds. |
| `OnBudget.graceful_exit` | Emits `budget_graceful_exit`; returns `{status: "budget_exhausted", strategy: "graceful_exit", reason: "on_budget_graceful_exit", budget_state, message}`. The pipeline's `on_finish` chain can drain in-flight work and surface the envelope. |
| `OnBudget.warn_and_continue` | Emits `budget_warn_and_continue`; injects a 1-turn `budget_warning` system_reminder via `tool_hooks_inject_reminder`; passes the original `budget_state` through unchanged. |

`OnBudget()` returns the namespace dict so dotted access works after
one import:

```harn
import { OnBudget } from "std/lifecycle/on_budget"
import { compose, with_telemetry } from "std/lifecycle/combinators"

fn audit_overage(harness, budget_state) {
  harness.emit_audit("custom_overage_logged", {state: budget_state})
  return budget_state
}

pipeline default() {
  register_persona_hook(
    "*",
    "OnBudgetThreshold",
    compose([OnBudget.warn_and_continue, with_telemetry(audit_overage)]),
  )
  return "ok"
}
```

## Hook events

Pipeline lifecycle gates fire on top of the same session-hook surface
as the rest of the system. Register with
`register_session_hook(event, handler)`. Veto with
`{block: true, reason}`; amend the dispatched payload with
`{modify: payload}`.

### Pipeline-finish gates

| Event | Allow | Deny / Block | Modify | Reminder |
|---|---|---|---|---|
| `pre_finish` | yes | INVALID — use `OnFinish.block_until_settled` | n/a | inject only |
| `post_finish` | yes | n/a (advisory) | n/a | inject only |
| `on_unsettled_detected` | yes | block finish until settled | amend unsettled payload | inject only |

`pre_finish` rejects `{block: true}` and surfaces a runtime error
pointing at `OnFinish.block_until_settled` so the right primitive
fires before the value is captured.

### Suspend / resume gates

| Event | Allow | Deny / Block | Modify | Reminder |
|---|---|---|---|---|
| `pre_suspend` | yes | cancel suspend, worker keeps running | rewrite reason | inject only |
| `post_suspend` | yes | n/a | n/a | inject only |
| `pre_resume` | yes | stay suspended | amend resume input | inject only |
| `post_resume` | yes | n/a | n/a | inject only |

See the agent-lifecycle entry in `docs/llm/harn-quickref.md` for the
underlying suspend/resume primitive (harn#1836).

### Drain gates

| Event | Allow | Deny / Block | Modify | Reminder |
|---|---|---|---|---|
| `pre_drain` | yes | skip drain | amend drain spec | inject only |
| `post_drain` | yes | n/a | n/a | inject only |
| `on_drain_decision` | yes | block tool call | rewrite tool call | inject only |

`on_drain_decision` fires once per item the settlement-agent loop
processes, so a hook can audit or override every disposition before it
persists.

### Full event table

The full 40-event hook taxonomy spans tool, persona, worker, and
session surfaces. The events most relevant to pipeline lifecycle:

| Event | Scope | Notes |
|---|---|---|
| `PreToolUse` / `PostToolUse` | tool | `register_tool_hook` |
| `PreAgentTurn` / `PostAgentTurn` | persona/session | persona dispatch |
| `WorkerSpawned` / `WorkerProgressed` / `WorkerWaitingForInput` | worker | `register_worker_hook` |
| `WorkerSuspended` / `WorkerResumed` / `WorkerCompleted` / `WorkerFailed` / `WorkerCancelled` | worker | terminal worker events |
| `PreStep` / `PostStep` | persona | per-stage |
| `OnBudgetThreshold` | persona | use `OnBudget.*` presets |
| `OnApprovalRequested` / `OnHandoffEmitted` | persona | persona-loop signals |
| `OnPersonaPaused` / `OnPersonaResumed` | persona | persona suspend |
| `SessionStart` / `SessionEnd` | session | bracket the session |
| `UserPromptSubmit` | session | accepts `{block, reason}` |
| `PreCompact` / `PostCompact` | session | context maintenance |
| `PostTurn` | session | after every agent turn |
| `PermissionAsked` / `PermissionReplied` | session | `{decision: "allow"\|"deny"\|"ask"}` |
| `FileEdited` | session | drained per-turn |
| `SessionError` / `SessionIdle` | session | observability |
| `PreFinish` / `PostFinish` | session | wraps `on_finish` |
| `OnUnsettledDetected` | session | non-empty harness at finish |
| `PreSuspend` / `PostSuspend` / `PreResume` / `PostResume` | session | lifecycle gates |
| `PreDrain` / `PostDrain` / `OnDrainDecision` | session | drain-loop gates |

Worker events do not support reminder effects — the runtime returns
`HARN-RMD-002` when a worker hook tries to inject one. Use a session
or persona hook instead.

## Replay determinism

Every lifecycle decision the runtime makes is recorded so a replay
reproduces the same control flow:

1. **Cached resume input.** `harness.resume_subagent(handle, input)`
   persists the input snapshot on the resume event so the replay
   oracle can feed the same value back into the same worker without
   re-reading host state.
2. **Memoized drain decisions.** Each `drain_decision` audit captures
   the bucket, item id, and disposition. The replay oracle replays the
   audit log instead of re-walking the snapshot, so a non-deterministic
   `on_drain_decision` hook (e.g. one that consults wall-clock) cannot
   drift the second run.
3. **Signed timestamps.** `harness.emit_audit` stamps entries with a
   per-run monotonic `seq` rather than wall-clock time, so audit
   ordering is reproducible. Wall-clock fields (`queued_at_ms`,
   `age_ms`) come from `clock_mock`-aware sources and respect
   `mock_time(...)` / `advance_time(...)` in tests.
4. **One-shot registration.** `pipeline_on_finish(callback)` is
   last-write-wins, and the slot is consumed exactly once per run via
   `take_pipeline_on_finish`. A stale registration cannot leak across
   runs.

## See also

- [Pipeline lifecycle presets](./stdlib/lifecycle.md) — per-preset
  surface, harness method reference.
- [Lifecycle cookbook](./cookbooks/lifecycle.md) — five end-to-end
  patterns (nightly handoff, custom audit, long-paused agent,
  business-hours suspend deny, replay-deterministic test harness).
- [Hooks](./extensibility/hooks.md) — tool, persona, and session hook
  registration surface.
- [Agent channels](./agent-channels.md) and [Agent pools](./agent-pools.md)
   — sibling epics whose work surfaces in
  `harness.unsettled_state()`.
- The `LLM quick reference` Pipeline lifecycle section — copy-paste
  presets, combinators, and event tables.
