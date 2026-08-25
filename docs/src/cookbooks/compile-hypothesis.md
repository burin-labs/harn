# Compile a bounded experiment from a hypothesis

Use `std/eval/hypothesis` when an agent or product needs to turn a question into
an experiment without letting model output become executable authority. The
planner produces typed data. The deterministic compiler accepts only registered
adapters, validates trusted host risk, capability, placement, citation, and
resource ceilings, and lowers an accepted design into Harn's existing
experiment registration contract.

This guide shows the control flow. The complete executable fixture is
[`eval_hypothesis_compiler.harn`](../../../conformance/tests/stdlib/eval_hypothesis_compiler.harn),
with its catalog and intent in
[`hypothesis_fixture_lib.harn`](../../../conformance/tests/stdlib/hypothesis_fixture_lib.harn).

## Register what the experiment may do

Build an `ExperimentCompileContext` at the host boundary. Its catalog contains
the only intervention, outcome, and population adapters the compiler may
reference. Its budget ceiling is authority; a requested budget above any
ceiling is refused.

```harn,ignore
import { compile_experiment_intent } from "std/eval/hypothesis"

const context = {
  owner: "eval-team",
  seed: "release-42",
  catalog: {
    schema: "harn.experiment.adapter_catalog.v1",
    interventions: [baseline_adapter, candidate_adapter],
    outcomes: [bounded_success_outcome],
    populations: [frozen_fixture_population],
  },
  trusted_citations: researched_sources,
  risk_floor: host_classified_risk,
  capability_ceiling: host_capability_ceiling,
  supported_blocking_factors: ["host", "time_slot"],
  host_identity_available: true,
  budget_request: requested_budget,
  budget_ceiling: approved_ceiling,
  approval_id: nil,
  provenance: {
    source: "release-eval",
    actor: "automation",
    created_at: "2026-08-08T00:00:00Z",
  },
}

const receipt = compile_experiment_intent(intent, context)
if !receipt.ok {
  for diagnostic in receipt.failure?.diagnostics ?? [] {
    harness.stdio.eprintln(diagnostic.code + ": " + diagnostic.message)
    if diagnostic.repair != nil {
      harness.stdio.eprintln(
        "repair " + diagnostic.repair.owner
          + " " + diagnostic.repair.operation
          + " " + diagnostic.repair.path,
      )
    }
  }
  return
}
```

The current authored contract is `harn.experiment.intent.v2`. It carries typed
alternative hypotheses and a typed decision scope. The compiler also accepts
v1 input and normalizes it before fingerprinting, so equivalent v1 and v2
intents produce one plan identity. Future versions fail with
`experiment.intent_schema_version`; structural failures use
`experiment.intent_schema`. Both can carry a typed repair owner, operation,
path, and expected shape.

Select `quasi_experimental` only with an explicit `quasi_experiment` contract.
Name the non-random assignment mechanism and identifying assumptions. A
matched comparison must also name its matching method and pre-treatment
covariates. The current compiler emits an `observe_only` plan with an
`associational` claim ceiling; it does not reinterpret matching as randomized
causal evidence.

Use immutable adapter IDs and variants. Put credentials in host-managed secret
references, never in the catalog or intent. The capability manifest declares
filesystem roots, process commands, network domains, providers, connectors,
database scopes, customer-state access, model routing, privacy class and
retention, mutation reversibility, and approval requirements. Missing
customer-state, model, and privacy fields in a legacy manifest normalize to no
authority. New compiled plans carry them explicitly. Executable
adapters may also declare one typed `placement` requirement with `mode`,
`platform`, `requires_gpu`, and `resource_class`. Baseline and candidate
requirements must be identical, and the compiler refuses a host ceiling that
cannot enforce them.

## Hand the registration to an enforcing host adapter

An accepted receipt contains a stable intent fingerprint and plan fingerprint.
If `receipt.plan.kind == "registered_experiment"`, the plan contains the
canonical `ExperimentManifest` and `ExperimentRegistration`. It deliberately
does not contain an executable workflow. Only a registered host adapter that
enforces the plan's capabilities, placement, approval requirement, and
remaining resource ceilings may schedule it; that adapter must use Harn's
canonical assignment, observation, and decision APIs rather than reconstructing
their rules.

```harn,ignore
if receipt.plan.kind == "observe_only" {
  for question in receipt.plan.instrumentation_questions {
    harness.stdio.println("instrumentation needed: " + question)
  }
  return
}

const plan = receipt.plan
if plan.design.approval_required {
  request_native_approval(plan.design.approval_id, plan.fingerprint)
  return
}

require plan.execution_status == "requires_registered_host_adapter",
  "compiled registrations are not self-executing"
registered_experiment_adapter.schedule(plan.registration, {
  capabilities: plan.design.capabilities,
  resource_ceiling: plan.design.budget,
  plan_fingerprint: plan.fingerprint,
})
```

`observe_only` is a successful, non-executable result. It preserves the
question and names missing instrumentation instead of manufacturing a causal
test. A high-risk randomized design carries a stable approval requirement;
compilation does not pretend approval already happened, and an adapter must not
execute it without the corresponding native approval. Record that approval as
an `approval_recorded` event bound to the exact approval ID and plan
fingerprint, then mint its opaque `native_approval` proof from the registered
approval adapter. The ledger refuses scheduling before the matching approval.
The event payload is an audit record, not evidence that the native approval UI
ran.

## Record lifecycle facts once

Create typed events with `hypothesis_event`, append them with
`hypothesis_ledger_append`, and derive current state with the single-pass
`hypothesis_ledger_snapshot`. The ledger is a typed projection over Harn's event
log, so it inherits global ordering, integrity hashes, and SQLite, file, or
memory persistence. The topic is reserved: generic event-log writes fail, and
the specialized append requires a non-serializable authority proof minted by a
registered native adapter for that exact event.

```harn,ignore
import {
  hypothesis_event,
  hypothesis_ledger_append,
  hypothesis_ledger_snapshot,
} from "std/eval/hypothesis"

const event = hypothesis_event({
  schema: "harn.hypothesis.event.v1",
  schema_version: 1,
  event_id: "plan-registered",
  hypothesis_id: hypothesis_id,
  plan_id: plan.plan_id,
  run_id: nil,
  predecessor_fingerprint: nil,
  occurred_at: "2026-08-08T00:01:00Z",
  actor: "automation",
  source: "release-eval",
  payload: {kind: "plan_registered", plan: plan},
})

// The native adapter owns this receipt
// and returns a tagged success only after
// it verifies that the corresponding plan-admission operation completed.
const proof = harness.obs.hypothesis_event_authority_request(
  "plan_admission",
  event.fingerprint,
  plan.fingerprint,
  hypothesis_id,
  "plan-admission-receipt-01",
  nil,
)
const first = hypothesis_ledger_append(harness.obs, event, proof)
const replay = hypothesis_ledger_append(harness.obs, event, proof)
require first.cursor == replay.cursor && !replay.inserted,
  "a retry must return the original durable event"

const read = hypothesis_ledger_snapshot(harness.obs, hypothesis_id)
require read.integrity.scope == "retained_topic_chain"
  && read.integrity.verified,
  "the retained topic chain must verify before projection"
const snapshot = read.snapshot
```

The example assumes the host registered `hypothesis.attest_event` and gave this
pipeline the narrow `authority.write@plan_admission` effect grant. Harn sends
the exact authority kind, fingerprints, IDs, and operation receipt over the
host bridge. The adapter must return a JSON-RPC error for a missing, stale,
mismatched, reused-with-different-bindings, or denied receipt. It may return the
same success for an exact retry. An accepted response is the exact tagged result
documented in [Bridge protocol](../bridge-protocol.md#native-hypothesis-attestation).
Harn consumes that document inside the scoped builtin and returns only a
non-serializable VM resource. An ordinary `host_call` sees the same document as
plain data and cannot turn it into authority.

In-process Rust embedders may instead create a native attestation with
`harn_vm::stdlib::mint_hypothesis_native_attestation` and pass it to
`hypothesis_event_authority_mint`. That constructor is not a wire format.

Do not grant
hypothesis-event authority writes to model-authored code. Give approval,
execution, and lifecycle adapters only their corresponding
`authority.write@native_approval`, `authority.write@native_observation`, or
`authority.write@lifecycle_audit` scope. Mint
`native_approval` only after native approval, `native_observation` only after an
assigned execution produces its measurement, and `lifecycle_audit` only from
the adapter that owns the transition or decision. Every proof is bound to the
event fingerprint, plan fingerprint, hypothesis, and optional run; copying its
serialized audit headers cannot authorize another append.

Each later event names the preceding aggregate fingerprint. Reusing the same
hypothesis and event IDs with different content, breaking that predecessor
chain, recording an unassigned observation, exceeding the plan budget, or
submitting a decision that differs from Harn's canonical recomputation fails
closed. Record realized paired observations, execution drift, decisions,
invalidations, regressions, and follow-up relationships as events; do not update
a parallel JSON document. Reports and host dashboards should project the ledger
snapshot rather than becoming independent sources of truth.

A `completed` run transition carries typed completion evidence. Use
`{kind: "statistical"}` only when `decide_experiment` is already non-`RUNNING`
with `budget_spent: false`. Use `{kind: "max_trials"}` only after every frozen
candidate, case, and trial cell has an observation. Native budget and wall-clock
completion carry a non-empty receipt ID in the fingerprinted transition and in
its `receipt_ids`; the native lifecycle attestation binds that exact event. The
ledger validates the event and its attestation. It does not query or validate
the host's external receipt store.

Use `hypothesis_workflow(harness.obs, {kind: "inspect", hypothesis_id: id})`
for the same typed read and report projection. `start`, `pause`, `resume`,
`advance`, and `stand_down` are state-checked requests. They call the registered
`hypothesis.operation` native adapter; when none is present, they return
`kind: "adapter_unavailable"` before appending a lifecycle event.

`advance` executes one balanced case/trial block. The caller supplies concrete
blocking values, Harn freezes and randomizes the arm order with
`plan_assignments`, and the adapter returns measurements in exactly that order.
Harn realizes the assignments, validates the full block against the resource
ceilings, appends only missing cells on retry, and runs `decide_experiment`.
The adapter never chooses the assignment or supplies the verdict.

```harn
const advanced = hypothesis_workflow(
  harness.obs,
  {
    kind: "advance",
    hypothesis_id: plan.hypothesis_id,
    blocking_values: {host: host_id, time_slot: frozen_slot},
  },
)
```

Call `design_hypothesis` separately when natural-language design is needed.

## Use the same boundary from CLI or MCP

The [hypothesis control-plane example](../../../examples/hypothesis-control-plane/README.md)
exports the planner, compiler, workflow, ledger inspection, and report as typed
functions. Run one function through `harn run`, or expose the same functions as
structured tools with `harn serve mcp`. The projection adds no scheduler,
manifest, decision rule, or write authority. The `compile` tool's generated
schema accepts the same v1/v2 intent union, including the v2 quasi-experiment
and decision-scope fields.

## Verify the boundary you claim

For deterministic compiler changes, run the exact conformance fixture:

```console
harn test conformance tests/stdlib/eval_hypothesis_compiler.harn --verbose
```

For an experiment claim, also prove through the native adapter that the registered intervention fired,
the realized assignment matched the randomized plan, outcomes came from the
declared population and grader, the stopping decision used the frozen
experiment registration, and resource totals remained below every ceiling.
Passing compiler tests alone does not prove a live intervention worked.
