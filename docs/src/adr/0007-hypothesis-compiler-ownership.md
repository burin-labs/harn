# ADR 0007: compile hypotheses into Harn's experiment-registration owner

## Status

Accepted on 2026-08-08 for
[#6353](https://github.com/burin-labs/harn/issues/6353).

This decision defines the ownership boundary for the first Hypothesis Compiler
vertical slice. It does not claim support for observational causal inference or
hosted execution.

## Context

Harn, Burin, and Harn Cloud each have useful parts of an experimentation
product, but their current hypothesis shapes do not form one lifecycle:

- Harn has free-form agent hypotheses, controlled-experiment contracts,
  randomized-block assignment, anytime-valid bounded decisions, task-plan
  compilation, capability policy, and an integrity-chained event log with
  SQLite, file, and memory backends.
- Burin has product-specific flag, prompt, metric, placement, approval, and
  launcher adapters. Its native hypothesis store is a temporary JSONL shim.
- Harn Cloud has tenant-scoped `human_hypotheses` and append-only outcomes, but
  manually repeats status and request types across SQL, Rust, Harn, and
  TypeScript.

Building a new end-to-end engine in any one host would duplicate at least an
experiment runtime, scheduler, or evidence ledger. It would also make a natural
language plan an authority-bearing program, which is the wrong trust boundary.

The existing persona compiler provides the relevant seam: a model returns a
narrow typed candidate; deterministic Harn code validates and normalizes it
into an existing runtime contract. The controlled-experiment modules already
own registration, assignment, immutable observations, and decisions. A host
adapter must separately prove it can enforce the compiled capabilities,
approval, and total-resource ceilings before it executes that registration.

## Falsifiers

This decision is wrong if any of the following is required for the vertical
slice:

1. The compiler cannot express a bounded randomized controlled experiment
   without host-specific fields in the portable contract.
2. An accepted plan cannot lower into the existing `ExperimentRegistration`
   owner without introducing a second experiment model.
3. The event log cannot provide retry-safe append, chain verification, and
   replay without a second local database.
4. Burin or Cloud must reinterpret a portable decision to render it or enforce
   an approval.
5. The same fixture cannot round-trip through the local ledger and the hosted
   projection without losing identity, provenance, or event order.

If a falsifier is observed, revisit the owning contract. Do not hide the mismatch
behind a host-only compatibility model.

## Decision

### One portable compiler boundary

Harn will own versioned `HypothesisSpec` and `EvidencePolicy` contracts. The
first accepted evidence lane is a randomized controlled experiment with bounded
metrics. A model may propose a constrained candidate, but deterministic code
owns:

- schema validation and normalization;
- stable identity and content fingerprints;
- lane selection and causal-claim classification;
- metric bounds, estimand, practical threshold, evidence ceilings, and frozen
  multiplicity family;
- capability, approval, and resource-budget validation against trusted host
  ceilings;
- compilation into `ExperimentRegistration`;
- refusal when a warning would weaken an authority or evidence boundary.

The compiler never emits Harn source code. Model output is data, not executable
authority.

### Evidence lanes are explicit

The first slice accepts only the randomized controlled lane for decisions.
Observational input may be preserved as an `observe_only` hypothesis, but it
cannot flow through `ExperimentDecision` or auto-promote a causal conclusion.
Supporting observational causal inference later requires its own typed design,
estimand, assumptions, diagnostics, and sensitivity-analysis contract.

Prior belief, observed evidence, statistical decision, and product utility are
separate fields and events. A confidence label cannot stand in for all four.

### Existing runtimes remain the owners

- `std/eval/experiment` owns controlled registration, assignment, observations,
  promotion between iterate and gate splits, and decisions.
- `std/eval/sequential` owns anytime-valid bounded inference.
- A registered host adapter owns execution structure and must enforce the
  compiled capabilities, approval requirement, and resource ceilings before it
  calls the canonical assignment and decision APIs. The first slice does not
  synthesize an execution graph.
- Harn's event log owns global ordering, atomic idempotency, integrity chaining,
  SQLite/file/memory persistence, subscriptions, compaction, and replay. The
  session store remains the owner of agent transcripts, not experiment state.
- Hosts own native presentation, concrete mutations, approvals, placement, and
  execution adapters.

No hypothesis-specific scheduler, workflow runtime, statistical library, or
SQLite database is introduced.

### One event vocabulary, many projections

The first portable ledger slice is an append-only stream of versioned events:

- plan registered;
- host approval or denial recorded when required;
- run state transitioned;
- assignment-bound observation recorded;
- canonical decision recorded;
- relationship recorded, including follow-up links;
- execution drift, invalidation, or later regression recorded.

Capture, compile/refusal, and promotion proposal/application events are the next
vocabulary additions. Hosts must not encode them as invented variants of the
first-slice union.

Each event carries a stable aggregate ID, event ID, schema version, logical
idempotency key, provenance, payload fingerprint, and integrity predecessor.
The `hypotheses.events.v1` topic is reserved from the generic event-log write
APIs. A registered native adapter must mint an opaque hypothesis-event authority
proof for the exact event fingerprint, plan fingerprint, aggregate, run, and
authority kind before the specialized append API accepts it. Plan admission,
native approval, native observation, and lifecycle audit are distinct authority
kinds. Serialized event JSON and audit headers record provenance; neither is a
reusable grant of authority.

The host must withhold the non-serializable native attestation from
model-authored code and issue it only through a registered adapter after the
owning native operation succeeds. The Harn mint boundary requires that opaque
attestation in addition to its resource-scoped execution-policy grant. In
particular, a structurally valid `approval_recorded`
payload does not prove that approval UI ran, and a structurally valid
`observation_recorded` payload does not prove that an assigned intervention
executed. The opaque proof is non-serializable and bound to one exact execution
scope. Resource-scoped `authority.write@<kind>` effects let an execution-policy
ceiling grant plan admission, native approval, native observation, and lifecycle
audit independently; a connector grant is not authority to mint these proofs.

Protocol hosts use the same boundary without serializing the resource. The
`hypothesis_event_authority_request` builtin sends the exact bindings plus a
native operation-receipt ID through `hypothesis.attest_event`. The host either
returns a JSON-RPC error or the exact `harn.host-result.v1` success marker. Harn
consumes that marker inside the authority-scoped builtin and mints the resource
in the current VM execution. The generic host-call path deliberately leaves an
identical marker as ordinary data, so `connector.call` cannot substitute for
`authority.write@<kind>`. Requests bypass script mocks and the per-turn read
memo because attestation is an operation-completion boundary.

After checking that proof, the hypothesis ledger validates the typed payload
and aggregate predecessor before using the existing event log's atomic
compare-and-append. It folds a read model from the same events, reapplies
admission during replay, and verifies the retained underlying integrity chain
when it reads. JSONL is an export, not a competing writer.

Run completion is a closed contract rather than a free-form reason. Statistical
completion requires a non-`RUNNING` canonical decision computed without
pretending the budget is spent. Max-trial completion requires every frozen
candidate-by-case-by-trial cell. Native budget and wall-clock completion require
a receipt ID inside the fingerprinted, natively attested lifecycle event. Harn
validates that binding; the host remains responsible for the external operation
receipt store.

The portable workflow projection is intentionally read-first. It can inspect
the verified ledger and classify `start`, `resume`, and `stand_down` against the
current state. This slice has no native operation adapter, so mutation requests
return a typed `adapter_unavailable` result and append no lifecycle event. This
is not an execution scheduler or queue.

Integrity verification proves self-consistency from the retained topic genesis
to its retained head. It detects edits to retained payloads and provenance, but
without a separately trusted checkpoint it cannot detect tail deletion or a
fully rehashed rewrite. Reports and operational claims must preserve that
distinction.

Evidence aggregates cannot be truncated or hard-deleted through the portable
adapter. Archive and retention are explicit policy events with preserved proof.

### Host and Cloud migration

Burin replaces its JSONL hypothesis shim with the native Harn event-log
projection. Its existing experiment launcher remains a product adapter after it
consumes Harn assignment plans and emits realized-assignment receipts. Native
approval applies a promotion proposal and records an application receipt; a
developer override is not a product graduation.

Harn Cloud evolves `human_hypotheses` and `human_hypothesis_outcomes` in place
as tenant-scoped projections. Existing routes remain compatibility views during
a named migration window. A transactional projector appends an event, updates
the current snapshot, and links receipts or learning-loop relationships without
dual writes. Hosted execution depends on the existing durable-runner owner; the
hypothesis service does not add a scheduler.

Rust, Harn, SQL, OpenAPI, SDK, Zod, and portal shapes are generated or
mechanically checked from one versioned contract registry. Manually repeated
enums are migration debt, not new extension points.

### Commercial boundary

Local design, validation, synthetic replay, local execution, and a verified
local ledger remain useful without a paid entitlement. Named entitlements may
gate shared hosted history, collaboration, governance and retention, fleet
scheduling, or managed execution. Tier order never substitutes for a named
capability, and commerce does not own experiment semantics.

## Acceptance evidence

The vertical slice is complete only when the canonical path proves all of the
following:

1. Plain language produces a schema-bound candidate and a deterministic plan
   with a stable fingerprint.
2. The plan lowers into the existing experiment-registration contract with
   explicit capability and total-resource ceilings and an honest
   `requires_registered_host_adapter` execution state.
3. Registration, synthetic randomized observations, a typed decision, and a
   follow-up survive process restart in the integrity-chained SQLite event log.
4. Duplicate appends replay idempotently; concurrent stale-head appends and
   revised, reordered, cross-run, degraded, or unassigned observations are
   refused.
5. Retained-chain verification fails after payload or provenance tampering and
   succeeds on the unmodified history; tail-deletion detection remains outside
   the claim until an external authenticated checkpoint exists.
6. A live A/A control never promotes and a known-bad arm loses under a
   predeclared spend ceiling, with effect-reachability and host-condition
   receipts proving the intended path fired.
7. The same events project through Burin and Cloud without manual semantic
   reinterpretation or loss of tenant, provenance, receipt, or relationship.
8. Pause, cancel, crash, and resume preserve evidence and enforce remaining
   spend. A UI-only cancellation flag is not sufficient.
9. Early stopping cannot become exhaustion by setting a boolean: statistical,
   exact max-trial, native budget, and wall-clock completion each satisfy their
   own typed admission rule.

Test counts, snapshots, and simulated launcher metadata do not establish these
claims.

## Consequences

- The first slice is intentionally deep but narrow: randomized bounded metrics
  are decisive; observational causal inference and hosted execution are not.
- Existing Harn experiment contracts may gain versioned fields and event
  descriptors, but their decision and assignment semantics remain authoritative.
- The event log's typed hypothesis projection needs conflict-detecting
  idempotency and an evidence-retention policy before it can claim
  organizational-memory durability.
- Burin and Cloud migrations delete handwritten projections after compatibility
  consumers move; they do not maintain parallel permanent models.
- Later evidence lanes can extend the compiler registry without changing the
  trust boundary or execution owner.

## Evidence

- Harn controlled-experiment foundations:
  [#5669](https://github.com/burin-labs/harn/pull/5669) and
  [#5682](https://github.com/burin-labs/harn/pull/5682).
- Harn persona compilers in `std/personas/{prompt_compiler,compiler}`.
- Harn event-log ordering, idempotency, integrity-chain, SQLite, compaction, and
  replay support in `crates/harn-vm/src/event_log` and `provenance`.
- Burin experiment adapters under `scripts/lib/experiment` and Harn adoption in
  [burin-code#5547](https://github.com/burin-labs/burin-code/pull/5547).
- Harn Cloud's existing hosted aggregate was introduced in
  [harn-cloud#117](https://github.com/burin-labs/harn-cloud/pull/117); durable
  hosted execution remains tracked by
  [harn-cloud#679](https://github.com/burin-labs/harn-cloud/issues/679).
