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
