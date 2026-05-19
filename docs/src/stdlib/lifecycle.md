# Pipeline lifecycle presets (`std/lifecycle`)

The pipeline DSL accepts a single `on_finish` callback that runs after
the pipeline's declared steps complete and before the pipeline returns
its value to the host. The callback signature is
`fn(harness, return_value)` and its return value replaces the pipeline's
return value. Register a callback with the global `pipeline_on_finish`
builtin from anywhere inside the pipeline body.

```harn
import { on_finish_drain } from "std/lifecycle"

pipeline default() {
  pipeline_on_finish(on_finish_drain)
  return "ok"
}
```

## Harness lifecycle methods

`harness.unsettled_state()` returns one snapshot dict with four lists:
`suspended_subagents`, `queued_triggers`, `partial_handoffs`, and
`in_flight_llm_calls`. Suspended subagents come from the host worker
registry, queued triggers come from the trigger inbox and worker queue
event-log records, partial handoffs come from `harness.handoff_to`, and
in-flight LLM calls come from the live LLM call registry.

`harness.is_empty(state?)` returns `true` when every unsettled bucket is
empty. Pass an already-captured snapshot to keep decisions consistent
inside one callback, or omit the argument to ask the harness for a fresh
snapshot.

`harness.counts(state?)` returns `{suspended, queued, partial,
in_flight}` for either the supplied snapshot or a fresh harness snapshot.
Use it for audit payloads and branch decisions instead of recomputing
bucket lengths manually.

`harness.summary(state?)` returns a single operator-readable line such
as `no unsettled work` or a per-bucket count summary. It is intended for
logs and audit records, not as a machine-readable contract.

`harness.resume_subagent(handle, input?)` delegates to the host worker
resume primitive for suspended workers. When `input` is supplied and the
worker is an awaiting retriggerable subagent, the harness falls back to
the existing send-input primitive.

`harness.cancel_subagent(handle, reason?)` delegates to
`__host_worker_close` and returns that primitive's worker summary or
error. The optional reason is reserved for callers that want to include
the reason in their own audit payload before cancelling.

`harness.handoff_to(target_pipeline, payload?)` records a partial
handoff envelope and returns `{status: "queued", envelope}`. The envelope
appears in subsequent `partial_handoffs` snapshots with `from`, `to`,
`payload_summary`, `queued_at_ms`, and `age_ms` fields.

`harness.acknowledge_trigger(id)` settles one queued trigger item. Worker
queue items are acknowledged with the existing queue ack event, while
trigger inbox items are settled by appending the existing dispatcher
cancel request; missing ids return `{status: "not_found"}`.

`harness.defer_trigger(id, target_pipeline?)` acknowledges the queued
trigger and records a partial handoff envelope for the supplied target
(default `deferred-triggers`). If the trigger cannot be acknowledged,
the returned receipt keeps the failed acknowledgement result.

`harness.acknowledge_handoff(envelope_id, decision?)` removes a partial
handoff envelope from the unsettled registry and emits a
`handoff_acknowledged` lifecycle audit entry with the caller's decision
payload.

`harness.wait_for_any_settlement(max_duration?)` takes a fresh snapshot
and returns `{status, timed_out, state}`. It returns `settled` when the
snapshot is empty and `unsettled` otherwise; callers that need a richer
drain loop should delegate to the drain preset or settlement agent.

`harness.emit_audit(kind, payload?)` records a typed
`LifecycleAuditEntry` with a monotonic per-run `seq` number. When an
active event log exists, the same entry is persisted to the
`pipeline.lifecycle.audit` topic alongside run-record data.

`harness.finalize(disposition?)` stores the pipeline disposition for the
current lifecycle run and emits a `pipeline_finalized` audit entry. The
receipt is shaped `{status: "finalized", method: "finalize", entry}`.

`harness.spawn_settlement_agent(unsettled, return_value)` is the handoff
point used by `on_finish_drain` when unsettled work remains. The
settlement-agent loop and constrained drain tool surface are tracked by
harn#1856; until that lands this method returns a typed unsupported
receipt rather than silently dropping the work.

`harness.current_pipeline_id()` returns the current run id when a
mutation session is installed, otherwise the session id, otherwise
`nil`. Handoff producers use this to stamp the origin pipeline.

Four canonical presets ship from `std/lifecycle`. Each is a pure
function (or a pure factory returning one) with no captured state, so
chaining and reuse are safe.

## `on_finish_abandon(harness, return_value)`

Reproduces today's no-callback behavior, but emits a
`pipeline_abandoned_unsettled` audit entry when unsettled state is
non-empty so the lost work is at least observable. Returns
`return_value` unchanged.

Use this preset when the pipeline's contract is "fire and forget" —
deferred work that survives the pipeline's exit is acceptable and any
downstream cleanup is the host's responsibility.

```harn
import { on_finish_abandon } from "std/lifecycle"

pipeline default() {
  pipeline_on_finish(on_finish_abandon)
  return "ok"
}
```

## `on_finish_drain(harness, return_value)`

The recommended default. Scans the harness unsettled state and either
finalizes the pipeline immediately (when nothing is deferred) or
delegates the per-item disposition to a settlement agent via
`harness.spawn_settlement_agent`. The settlement-agent loop itself
lands in harn#1856 (P-03); until that ticket ships, the harness method
returns a typed unsupported receipt and the preset surfaces that
receipt as the pipeline's return value so callers can detect the gap
deterministically.

```harn
import { on_finish_drain } from "std/lifecycle"

pipeline default() {
  pipeline_on_finish(on_finish_drain)
  return "triage complete"
}
```

## `on_finish_block_until_settled(timeout, fallback?)`

Returns a callback that asks the harness to wait for unsettled work to
drain naturally. If everything settles within `timeout`, the callback
emits `pipeline_finalized:settled_within_timeout` and returns the
unchanged `return_value`. On timeout it emits `settlement_timeout` and
delegates to `fallback` (default `on_finish_drain`). The fallback may
itself be any callback, so chains compose cleanly.

```harn
import { on_finish_block_until_settled } from "std/lifecycle"

pipeline default() {
  pipeline_on_finish(on_finish_block_until_settled(30s))
  return "ok"
}
```

## `on_finish_handoff_to(target_pipeline, options?)`

Returns a callback that packages the current unsettled-state snapshot
into a typed envelope (with `origin`, `unsettled`, and any
caller-supplied options) and hands it to a target pipeline via
`harness.handoff_to`. When there is nothing unsettled, the callback
short-circuits to `pipeline_finalized` and returns the unchanged
`return_value` — the typical case where the handoff pipeline does not
need to run at all.

```harn
import { on_finish_handoff_to } from "std/lifecycle"

pipeline default() {
  pipeline_on_finish(on_finish_handoff_to("nightly-drain"))
  return "triage complete"
}
```

## Composing presets

The factories accept other callbacks as their fallback / target
argument, so composition is just function nesting. A common production
chain — "wait briefly, otherwise hand off to a long-running pipeline,
otherwise drain immediately" — reads top-to-bottom:

```harn
import {
  on_finish_block_until_settled,
  on_finish_handoff_to,
} from "std/lifecycle"

pipeline default() {
  pipeline_on_finish(
    on_finish_block_until_settled(
      30s,
      on_finish_handoff_to("nightly-drain"),
    ),
  )
  return "ok"
}
```

Each preset emits typed audit entries via `harness.emit_audit`. The
entries live on a per-pipeline-run audit log that conformance fixtures
and replay oracles can drain via `lifecycle_audit_log_take()` (or peek
at without consuming via `lifecycle_audit_log_snapshot()`).

## Callback combinators (`std/lifecycle/combinators`)

The combinators in `std/lifecycle/combinators` wrap any
`(harness, return_value) -> return_value`-shaped callback — hook
handlers, `resume_by` callbacks, the presets above, and any custom
`on_finish` body. All six are pure factories that return closures with
no captured mutable state, so they nest freely:

- `compose([cb, ...])` runs each callback sequentially, threading the
  prior callback's return value into the next callback's
  `return_value`.
- `first_available([cb, ...])` invokes callbacks in order and returns
  the first non-nil result (fallback chains).
- `with_telemetry(cb, span_name?)` opens a `SpanKind::FnCall` OTel
  span and emits paired `{span_name}_started` / `_completed` /
  `_errored` audit entries.
- `with_timeout(cb, ms)` is a soft, clock-aware deadline. On overrun
  the wrapper returns a sentinel dict
  `{__timed_out: true, timeout_ms, elapsed_ms, return_value}` and
  emits a `lifecycle_callback_timed_out` audit.
- `if_unsettled(cb)` invokes the wrapped callback only when
  `harness.unsettled_state()` reports pending work (exactly one
  snapshot per call).
- `when(predicate, cb)` guards on an arbitrary
  `predicate(harness, return_value)` and short-circuits to the
  inbound return value when false.

A realistic composition wraps `on_finish_drain` with telemetry, a
soft deadline, and an unsettled-state guard so the drain only runs
when there is real work to do:

```harn
import {
  compose, if_unsettled, with_telemetry, with_timeout,
} from "std/lifecycle/combinators"
import { on_finish_drain } from "std/lifecycle"

pipeline default() {
  pipeline_on_finish(
    if_unsettled(
      with_telemetry(with_timeout(on_finish_drain, 30000), "drain"),
    ),
  )
  return "ok"
}
```

## Inspecting unsettled state directly

Custom `on_finish` callbacks have full access to the harness:

```harn
import { counts, summary } from "std/lifecycle"

pipeline default() {
  pipeline_on_finish(
    { harness, return_value ->
      let state = harness.unsettled_state()
      if !harness.is_empty(state) {
        harness.emit_audit(
          "custom_drain",
          {counts: counts(state), summary: summary(state)},
        )
      }
      return return_value
    },
  )
  return "ok"
}
```
