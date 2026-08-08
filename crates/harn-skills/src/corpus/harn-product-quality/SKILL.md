---
name: harn-product-quality
short: Launch-quality product behavior across Harn-powered surfaces.
description: Use one product contract across CLI, TUI, IDE, headless, and cloud projections.
when_to_use: Use when designing, reviewing, testing, or launching a user-facing Harn-powered workflow.
---

# Harn product quality

Use this skill when Harn behavior reaches a person through a CLI, TUI, IDE,
headless integration, or cloud control plane.

Pair it with [[harn-orchestration]] for execution design, [[harn-testing]] for
evidence, and [[harn-de-slop]] when surfaces have accumulated parallel policy.

## One product

Treat every surface as a projection of one execution contract.

- Harn owns orchestration, lifecycle, replay, evaluation, lineage, and audit.
- Hosts own native presentation, approval UX, mutations, and undo/redo.
- Harn Cloud owns durable remote execution without redefining Harn semantics.
- Keep lifecycle and completion meanings identical across projections.
- Put product-specific rendering behind adapters at a real seam.
- Do not parse prose to recover state the runtime already knows.
- Do not implement a second scheduler or approval engine in a host.
- Fix disagreement at the owner or projection interface.

## Canonical path

Design one default path before adding alternatives.

- A new user should discover it without reading architecture documentation.
- The path should demonstrate the product's central value in about a minute.
- Defaults should be safe and useful, not empty configuration homework.
- Show what will happen before consequential work starts.
- Show what is happening while work runs.
- Show what happened and where its evidence lives.
- Keep setup explicit when it cannot be eliminated.
- Test the path from a production-shaped package, not only source checkout.

## Control semantics

Control words are events, not suggestions.

- `stop` prevents future work promptly.
- `wait` parks without losing resumable state.
- `stand down` ends delegated or background work cleanly.
- `pivot` changes the active objective without completing stale work.
- Surface acknowledgment and resulting state.
- Preserve an auditable reason and initiator.
- Bound long-running tools so cooperative interruption has a turn boundary.
- Make restart and reconnect behavior explicit.
- Test controls during active work, not only at idle.

## Visible progress

Progress is structural state rendered as plain language.

- Report meaningful completed steps and the next active step.
- Prefer event-driven updates over polling or timed reassurance.
- Distinguish queued, running, waiting, blocked, failed, stopped, and complete.
- Do not call a task complete while required landing or release work remains.
- Preserve progress across reconnect, compaction, and process restart.
- Avoid exposing internal agent chatter as product status.
- Keep detailed traces available without making them the default UI.

## Evidence

Match evidence to the claim.

- Contract claim: inspect the schema, registry, or owning interface.
- Behavior claim: exercise the interface with a deterministic test.
- Product claim: run the canonical path end to end.
- Liveness claim: observe progress, interrupt, recover, and reach a terminal state.
- Quality claim: run multiple trials with a calibrated grader and threshold.
- Performance claim: measure a representative distribution and resource ceiling.
- Accessibility claim: verify keyboard, focus, labels, contrast, and reduced motion.
- Recovery claim: inject the failure rather than describing the fallback.

One good run is a demo. A test count is inventory. Neither proves a broad
quality claim.

## Plain language

- Lead with the user outcome.
- Name the object being changed and the scope.
- Explain approvals at the point they matter.
- Distinguish retryable failure from terminal failure.
- Give recovery actions that can actually be executed.
- Avoid internal protocol and model jargon in the primary path.
- Keep commands copyable and deterministic.
- Make destructive consequences unmistakable.

## Launch review

Before calling a workflow launch-ready, verify:

- one authoritative semantic owner;
- one discoverable canonical path;
- consistent behavior across supported projections;
- prompt stop, wait, stand-down, and pivot handling;
- deterministic progress and terminal states;
- recovery after cancellation, disconnect, and restart;
- approval and capability limits at the owning seam;
- accessibility on interactive surfaces;
- bounded time, token, concurrency, and monetary costs;
- observability and receipts sufficient for support;
- docs and help that match the shipped behavior;
- production-shaped end-to-end evidence.

## Reject

- A host-only fix for runtime semantics.
- A second source of truth "for convenience."
- Completion inferred from reassuring prose.
- A spinner used as liveness evidence.
- A screenshot used as workflow evidence.
- An approval prompt for ordinary in-scope reversible work.
- Autonomous production mutation without explicit authority.
- Retry loops without a deadline, budget, or terminal state.
- Surface polish that preserves cross-surface inconsistency.
- "Launch-ready" based only on unit tests.

## Verify

- Identify the module and interface that own each semantic decision.
- Exercise the default workflow in every supported projection.
- Compare emitted lifecycle states and receipts, not screenshots alone.
- Interrupt active work with every supported control event.
- Disconnect and resume from durable state.
- Inject representative provider, tool, network, and approval failures.
- Run deterministic tests through the owning interface.
- Run multiple stochastic trials for model-quality claims.
- Record the falsifier, evidence, and residual risk in the change.
