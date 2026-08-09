# Compile a bounded experiment from a hypothesis

Use `std/eval/hypothesis` when an agent or product needs to turn a question into
an experiment without letting model output become executable authority. The
planner produces typed data. The deterministic compiler accepts only registered
adapters, validates trusted host risk, capability, citation, and resource
ceilings, and lowers an accepted design into Harn's existing experiment
registration contract.

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
  }
  return
}
```

Use immutable adapter IDs and variants. Put credentials in host-managed secret
references, never in the catalog or intent. The capability manifest declares
filesystem roots, process commands, network domains, providers, connectors,
database scopes, mutation reversibility, and approval requirements.

## Hand the registration to an enforcing host adapter

An accepted receipt contains a stable intent fingerprint and plan fingerprint.
If `receipt.plan.kind == "registered_experiment"`, the plan contains the
canonical `ExperimentManifest` and `ExperimentRegistration`. It deliberately
does not contain an executable workflow. Only a registered host adapter that
enforces the plan's capabilities, approval requirement, and remaining resource
ceilings may schedule it; that adapter must use Harn's canonical assignment,
observation, and decision APIs rather than reconstructing their rules.

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
`hypothesis_ledger_append`, and derive current state with
`hypothesis_ledger_project`. The ledger is a typed projection over Harn's event
log, so it inherits global ordering, integrity hashes, and SQLite, file, or
memory persistence. The topic is reserved: generic event-log writes fail, and
the specialized append requires a non-serializable authority proof minted by a
registered native adapter for that exact event.

```harn,ignore
import {
  hypothesis_event,
  hypothesis_ledger_append,
  hypothesis_ledger_project,
  hypothesis_ledger_read,
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

// `native_attestation` is an opaque value injected only after the registered
// adapter completes plan admission. Ordinary Harn code cannot construct it.
const proof = harness.obs.hypothesis_event_authority_mint(
  native_attestation,
  "plan_admission",
  event.fingerprint,
  plan.fingerprint,
  hypothesis_id,
  nil,
)
const first = hypothesis_ledger_append(harness.obs, event, proof)
const replay = hypothesis_ledger_append(harness.obs, event, proof)
require first.cursor == replay.cursor && !replay.inserted,
  "a retry must return the original durable event"

const snapshot = hypothesis_ledger_project(
  hypothesis_ledger_read(harness.obs, hypothesis_id),
  hypothesis_id,
)
```

The example assumes it runs inside a registered plan-admission adapter with the
narrow `authority.write@plan_admission` effect grant. Do not grant
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

## Verify the boundary you claim

For deterministic compiler changes, run the exact conformance fixture:

```console
harn test conformance tests/stdlib/eval_hypothesis_compiler.harn --verbose
```

For an experiment claim, also prove that the registered intervention fired,
the realized assignment matched the randomized plan, outcomes came from the
declared population and grader, the stopping decision used the frozen
experiment registration, and resource totals remained below every ceiling.
Passing compiler tests alone does not prove a live intervention worked.
