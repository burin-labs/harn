# Engineering principles

Harn is the orchestration substrate for products built across command-line,
terminal, editor, headless, and cloud surfaces. Those surfaces should feel like
one product because they project the same underlying execution model, not
because they independently imitate one another.

These principles are decision tools. They apply to product design, runtime
architecture, agent instructions, tests, and release work.

## Ambitious outcomes, boring seams

Be ambitious about the durable user outcome and conservative about accidental
complexity. "Simpler" is not a reason to shrink the outcome. It means finding
the module that can own the complexity behind a small interface.

Prefer a deep module when removing it would force its behavior back into many
callers. Reject pass-through layers, speculative abstraction, and duplicated
policy. A real seam has multiple justified adapters, usually a production
adapter and a deterministic test adapter.

## One owner, many projections

Every semantic decision has one authoritative owner:

- Harn owns orchestration, lifecycle, replay, evaluation, worker lineage,
  capability policy, and durable audit semantics.
- Product hosts own native interaction, presentation, approval UX, concrete
  mutations, and undo/redo.
- Harn Cloud owns durable remote execution and control-plane concerns without
  redefining language or orchestration semantics.

CLI, TUI, IDE, headless, and cloud surfaces are projections or adapters. They
must not become parallel implementations of scheduling, policy, lifecycle, or
completion. If two surfaces disagree, repair the owner or the projection
contract rather than adding reconciliation logic.

## Structure over prose

Make important rules executable or structural:

- closed types instead of convention-shaped dictionaries;
- registries instead of duplicated lists;
- protocol fields instead of prompt parsing;
- generated projections instead of hand-synchronized copies;
- state machines and events instead of timing assumptions;
- checks that fail on drift instead of reminders to remember.

Prose explains intent. It is not the enforcement mechanism for a contract that
the repository can verify.

## Evidence follows the claim

Start with the claim, name a plausible falsifier, and gather the cheapest
evidence that could distinguish success from failure.

- A source or contract claim needs inspection and a focused structural check.
- A deterministic behavior claim needs a test through the owning interface.
- A user-flow claim needs the canonical path exercised end to end.
- A liveness claim needs progress, interruption, recovery, and terminal-state
  evidence, not only a happy-path snapshot.
- A stochastic quality claim needs multiple trials, calibrated grading, and a
  distribution or threshold; one impressive run is a demo, not a guarantee.

Do not report test counts as proof. Report which claims the evidence supports,
what remains untested, and whether the evidence came from the current source.

## Canonical path first

The default path is a product contract. It must be discoverable, short, and
complete enough to demonstrate the product's central value without hidden
setup. Optimize the canonical path before adding alternate entry points.

A release-worthy workflow should answer, in plain language:

1. What will happen?
2. What is happening now?
3. How do I stop, redirect, or recover it?
4. What happened, and what evidence supports that result?

The same answers should be available across product surfaces even when each
surface renders them natively.

## Autonomy with control

Within an approved scope, agents should research, implement, verify, recover,
and ship without asking for routine step-by-step permission. Pause when a
decision is genuinely ambiguous, destructive, outside scope, production
affecting, unusually expensive, or requires new authority.

Stop, wait, stand down, and pivot are control events, not conversational hints.
They must interrupt future work promptly, preserve enough state to resume or
hand off, and produce an auditable outcome. Recovery behavior belongs in the
owning harness or runtime rather than in a lucky prompt.

## Convergence over local polish

Prefer work that removes a class of inconsistency across surfaces over polishing
one surface in isolation. Before adding a local fix, ask:

- Which module owns this behavior?
- Which interface should every surface consume?
- Can one registry, event, schema, or generated artifact replace the copies?
- What check prevents the copies from returning?

Do not bundle unrelated speculative cleanup. Class-killing work is related when
it makes the requested outcome structurally true.

## Launch means operationally complete

"Implemented" is not the terminal state. Launch-shaped work includes the
canonical workflow, error and recovery behavior, accessibility, observability,
resource limits, docs, migration or compatibility decisions, and evidence from
the production-shaped path.

"Ship" means the change has landed on the intended main branch and required
post-merge or release checks have completed. A local diff, open pull request,
or green narrow test is an intermediate state.
