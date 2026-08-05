---
name: harn-de-slop
short: Remove duplicated policy, shallow seams, and weak contracts.
description: Converge Harn systems on one owner with typed interfaces and executable guards.
when_to_use: Use when a change adds wrappers, duplicated policy, open shapes, speculative compatibility, or cross-surface drift.
---

# De-slop Harn changes

Use this skill to remove accidental complexity without shrinking the durable
outcome. The goal is ambitious behavior behind boring seams.

Pair it with [[harn-orchestration]] for execution ownership,
[[harn-product-quality]] for cross-surface behavior, and [[harn-testing]] for
evidence.

## Start with the outcome

- State the user or operator outcome in one sentence.
- Name a plausible falsifier.
- Identify the module that should own the behavior.
- Identify every current copy or projection.
- Distinguish class-killing cleanup from unrelated speculative cleanup.
- Preserve launch requirements while simplifying the implementation.
- Do not use "simpler" to justify a smaller product.

## One owner

- A semantic decision gets one authoritative owner.
- Make other surfaces adapters or generated projections.
- Delete parallel scheduling, lifecycle, policy, and completion logic.
- Fix the owner or its interface when projections disagree.
- Prefer the existing Harn runtime and stdlib over host glue.
- Keep hosts responsible for native UX, mutations, and undo.
- Keep Harn Cloud responsible for remote durability, not language semantics.

## Deep modules

- Prefer a small interface hiding substantial behavior.
- Apply the deletion test: if removing the module spreads complexity across
  callers, the module is earning its keep.
- Reject pass-through wrappers that rename parameters without hiding policy.
- Put a seam where behavior actually varies.
- One adapter is a hypothetical seam; two justified adapters make it real.
- Keep internal test seams private to the implementation.
- Test through the same interface callers use.

## Closed contracts

- Replace open dictionaries with named records or enums.
- Use `Result<T, E>` for fallible operations.
- Give public functions explicit return types.
- Validate untrusted input once at the owning seam.
- Preserve diagnostic codes and actionable spans.
- Encode lifecycle and completion as typed states.
- Encode evidence and receipts as data, not prose conventions.
- Avoid `any` as a compatibility blanket.

## Structural guardrails

- Replace duplicated lists with one registry.
- Replace hand-synchronized files with generated artifacts.
- Add a structural guard when the regression is mechanically detectable.
- Wire every new guard into the repository's real gate.
- Keep guard input current-source-safe; do not false-pass on stale binaries.
- Prefer events and subscriptions over sleeps and polling.
- Prefer hash-guarded or structural edits when they materially reduce risk.
- Rebase and verify the final diff, not only the initial implementation.

## Compatibility

- Preserve compatibility only when a real consumer needs it.
- Name the consumer, migration path, and removal condition.
- Avoid dual-write or dual-read periods without a bounded cutover.
- Do not keep two semantic owners under a compatibility label.
- Fail loudly at unsupported boundaries.
- Update public docs and examples with the cutover.
- Add a regression at the owning interface.
- Remove dead compatibility once the migration condition is met.

## Orchestration

- Keep workflow logic in Harn when Harn already owns the capability.
- Use harness capabilities for host effects.
- Do not reimplement retry, lifecycle, replay, or audit in Rust or UI code.
- Bound parallel fan-out and resource use.
- Put recovery in the harness rather than in prompt luck.
- Make stop, wait, stand-down, and pivot structural events.
- Preserve durable state across reconnect and restart.
- Keep provider-specific policy behind provider adapters.

## Tests and evidence

- Start with the narrowest falsifying test.
- Test through the owning interface, not private implementation.
- Delete obsolete shallow-module tests after replacement coverage exists.
- Exercise the canonical user path for product claims.
- Exercise progress, interruption, recovery, and terminal state for liveness.
- Use multiple trials and calibrated grading for stochastic claims.
- Do not cite test counts as proof of broad correctness.
- Report current-source evidence and residual risk.

## Smells

- A new wrapper whose interface is as complex as its implementation.
- The same status mapping in CLI, TUI, IDE, and Cloud.
- Host code parsing assistant prose for state.
- A second registry added "temporarily."
- Retry loops without deadline, budget, or idempotency.
- New open dictionaries at a public seam.
- A screenshot standing in for end-to-end evidence.
- A rebase that invalidates earlier verification.
- Comments that promise a guard the repository does not run.
- A local polish fix that preserves ecosystem inconsistency.

## Review checklist

- Is the ambitious outcome still intact?
- Is there exactly one semantic owner?
- Is the interface smaller than the behavior it hides?
- Are types closed at public seams?
- Are projections generated or mechanically guarded?
- Can controls interrupt active work?
- Does recovery survive restart?
- Does evidence match each claim?
- Is the canonical path exercised?
- Is unrelated cleanup excluded?
- Are changelog, docs, and migration implications handled?
- Has the rebased final diff passed narrow and broad gates?
