---
name: harn-orchestration
short: Workflows, triggers, workers, handoffs, and lifecycle ownership.
description: Design one Harn execution substrate with bounded work and thin host projections.
when_to_use: Use for workflows, agent_loop composition, triggers, workers, handoffs, parallelism, or durable execution.
---

# Harn orchestration

Use this skill for workflows, triggers, workers, handoffs, parallelism, durable
execution, and `agent_loop` composition.

Pair it with [[harn-agent]] for loop behavior, [[harn-testing]] for
deterministic verification, and [[harn-product-quality]] for product projections.

## One execution substrate

- Harn owns scheduling, lifecycle, transcript, replay, lineage, and audit.
- A host adapts Harn state into native UI and concrete capabilities.
- Harn Cloud durably runs the same semantics; it does not fork them.
- CLI, TUI, IDE, headless, and cloud should consume one execution contract.
- Do not add a second scheduler, retry engine, or completion model to a host.
- Repair drift at the owner or projection interface.
- Keep provider-specific behavior behind provider adapters.

## Workflow design

- Model a workflow around a durable outcome and typed stages.
- Give every stage explicit input, output, failure, and compensation behavior.
- Keep the external interface small; hide composition inside a deep module.
- Use existing stdlib primitives before adding runtime mechanics.
- Keep pure decisions separate from host effects.
- Route effects through `harness.*`.
- Make idempotency and resume semantics explicit.
- Persist checkpoints before expensive or irreversible stages.

## Agent loops

- Use `AgentLoopOptions`, `agent_options`, or `agent_preset`.
- Bound iterations, tool duration, concurrency, tokens, and cost.
- Prefer structural progress and lifecycle events over prompt conventions.
- Define completion against observable artifacts or receipts.
- Use deterministic gates before model judges.
- Put resilience in a composable `llm_caller`.
- Keep a stable session id for durable work.
- Treat stop, wait, stand-down, and pivot as lifecycle events.

## Triggers

- Register typed trigger manifests with stable ids.
- Make match scope, deduplication, concurrency, and budget explicit.
- Store durable delivery and replay metadata.
- Use channels for publish/subscribe facts and handoffs for one recipient.
- Bound batching windows and partition keys.
- Make expiration behavior explicit.
- Verify replay produces the same decisions.
- Do not rely on host-local timers for durable semantics.

## Workers and handoffs

- Delegate a bounded outcome, not a vague role.
- Include scope, capability ceiling, budget, evidence, and return contract.
- Preserve parent/child lineage.
- Keep the parent responsible for integration.
- Suspend cooperatively at a bounded turn or tool seam.
- Graceful stop returns a typed handoff.
- Resume from durable state with continuity context.
- Do not widen authority through delegation.

## Parallelism

- Use `parallel each` for fail-fast independent work.
- Use `parallel settle` when all outcomes matter.
- Always set `max_concurrent` for broad fan-out.
- Bound queue size, retries, time, tokens, and monetary spend.
- Make ordering requirements explicit.
- Use idempotency keys for externally visible work.
- Preserve partial-success evidence.
- Avoid polling loops and real-time sleeps.

## Recovery

- Classify retryable and terminal errors with closed types.
- Cap retries and use explicit deadlines.
- Preserve the last accepted checkpoint.
- Make compensation or takeover behavior observable.
- Test process restart, reconnect, duplicate delivery, and stale resume.
- Keep recovery in the harness or runtime, not in a lucky system prompt.
- Emit terminal state when recovery is exhausted.
- Preserve enough evidence for an operator to continue.

## Host interface

- Expose typed state and commands, not internal orchestration details.
- Keep approvals native to the host but scoped by Harn policy.
- Keep mutations and undo/redo native to the host.
- Project progress, waiting, blocked, stopped, failed, and complete consistently.
- Avoid parsing logs or prose into product state.
- Make controls acknowledge their accepted state.
- Keep traces available for diagnosis without making them the primary interface.
- Test production and in-memory adapters through the same seam.

## Ownership questions

Before adding a mechanism, ask:

- Which module owns this semantic decision?
- Is this a new behavior or another projection?
- Can an existing event, registry, or state machine express it?
- Would deleting the new module spread useful complexity or remove indirection?
- Are there two real adapters that justify a seam?
- Can one typed contract replace copied policy?
- What current-source guard prevents drift?
- What evidence would falsify the design?

## Verify

- Check, lint, and format the script.
- Test the workflow through its public interface.
- Verify trigger deduplication and replay.
- Verify bounded fan-out and resource ceilings.
- Interrupt active work with stop, wait, stand-down, and pivot.
- Restart and resume from durable state.
- Inject provider, tool, approval, and network failures.
- Compare lifecycle states across every supported host projection.
- Run the canonical product path end to end.
