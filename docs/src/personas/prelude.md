# Persona Prelude

`std/personas/prelude` provides small orchestration primitives for persona
workflows. Each helper returns an explicit envelope with `ok`, `status`,
`result`, `error`, and `receipt` fields so persona code can compose control
flow while hosts persist the same audit shape.

Import the module with the usual selective import form, or with scoped import
sugar when a Rust-style path is easier to scan:

```harn
import {
  verify_then_act,
  bounded_loop,
  cheap_classify_then_escalate,
  parallel_sweep_with_circuit_breaker,
  with_audit_receipt,
  with_approval_gate,
} from "std/personas/prelude"
```

The scoped equivalent lowers `::` path segments into the same `std/...` module:

```harn
import std::personas::prelude::{verify_then_act}
```

All helpers accept an optional `options` dict. Common receipt fields are
`persona`, `step`, `trace_id`, `parent_run_id`, `input`, `inputs_digest`,
`outputs_digest`, `model_calls`, `cost_usd`, `handoffs`, `redaction_class`,
and `metadata`. Tests and replay fixtures can also pass `started_at` and
`completed_at` for deterministic receipts.

## Primitives

`verify_then_act(verifier, actor, options?)` runs a zero-argument verifier and
only calls the actor when the verifier returns an ok-shaped value. The verifier
may return `Ok(...)`, `{ok: true}`, a success `status`, or `true`; false, nil,
error, or failed status values produce `verification_failed` or
`verification_error` without running the actor. Use this for guarded operations
such as "verify CI and policy, then merge".

`bounded_loop(state_init, step_fn, options?)` runs `step_fn(state)` until the
step reports `{done: true}`, the iteration or duration budget is exhausted, or
`progress_required` is set and a step makes no progress. `state_init` may be a
state value or a zero-argument closure. `step_fn` may return the next state
directly or an envelope like `{state, done, progress}`. Use this for bounded
persona loops that must leave a clear stop reason instead of spinning.

`cheap_classify_then_escalate(input, cheap_model, escalate_model,
escalation_predicate, options?)` tries the cheap classifier first, then runs the
escalation model when the cheap result has `confidence < min_confidence`, when
the predicate returns true, or when the cheap route fails. Model specs can be
closures, option dicts passed to `llm_call`, or model names. Use this for triage
steps where most work should stay on a low-cost path but ambiguous cases need a
stronger model.

`parallel_sweep_with_circuit_breaker(items, step_fn, options?)` processes items
in `parallel settle` batches capped by `max_concurrent` and stops scheduling new
work once `failed >= max_failures`. The result includes per-item settled
records plus `succeeded`, `failed`, `skipped`, and `circuit_open`. If `on_break`
is a closure, it is called with the breaker summary. Use this for PR, issue, or
repo sweeps where a bad run should fail closed instead of burning through the
whole queue.

`with_audit_receipt(step_fn, options?)` returns a zero-argument wrapper that
runs `step_fn` and attaches a canonical `harn.receipt.v1` receipt whether the
step succeeds or throws. Use this around side-effecting or externally visible
steps that need uniform audit output.

`with_approval_gate(approval_kind, step_fn, options?)` returns a zero-argument
wrapper that requires an approval before running `step_fn`. Pass a recorded
`approval` in options for replay or set `request_approval: true` to ask the HITL
runtime for one; without either, the wrapper returns `status: "suspended"` and
does not block indefinitely. Denied approvals return `status: "denied"` with
the approval recorded in the receipt.
