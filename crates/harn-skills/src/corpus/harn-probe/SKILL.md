---
name: harn-probe
short: Evidence-driven investigation for material or unstable claims.
description: Choose the cheapest probe that can falsify a consequential codebase or runtime claim.
when_to_use: Use when a claim is uncertain, unstable, high-risk, or expensive to get wrong.
---

# Harn probe

Use this skill when a material claim is uncertain, temporally unstable,
high-risk, or expensive to get wrong. Do not probe by reflex when authoritative
source or an existing deterministic check already settles the question.

Pair it with [[harn-testing]] when the probe should become a regression and
[[harn-de-slop]] when investigation reveals duplicated ownership.

## Frame the claim

- State the claim precisely.
- Name the decision the claim affects.
- Name a plausible falsifier.
- Estimate the cost of being wrong.
- Check whether the fact is stable or likely to have changed.
- Identify the authoritative owner or interface.
- Stop if the claim is irrelevant to the requested outcome.

## Choose evidence by claim type

- Source claim: inspect the authoritative implementation or registry.
- Contract claim: inspect the type, schema, protocol, or generated artifact.
- Deterministic behavior claim: run a focused test through the public interface.
- Integration claim: exercise the real adapter against a controlled dependency.
- Product claim: run the canonical path end to end.
- Liveness claim: observe progress, interruption, recovery, and terminal state.
- Stochastic quality claim: run multiple trials with a calibrated grader.
- Performance claim: measure representative distributions and resource ceilings.

## Probe design

- Use the cheapest probe that can distinguish success from failure.
- Keep one variable changing at a time when possible.
- Capture inputs, environment, version, and current source identity.
- Prefer structured output over screenshots or prose.
- Bound time, concurrency, tokens, and monetary spend.
- Avoid production effects unless explicitly authorized.
- Use test adapters for destructive or expensive dependencies.
- Make the probe repeatable enough to become a regression when useful.

## Codebase investigation

- Start from the repository's authoritative registry or interface.
- Trace callers only far enough to validate ownership.
- Search for duplicated policy and parallel implementations.
- Distinguish generated projections from hand-maintained copies.
- Inspect blame, issues, or pull requests when intent is not in current source.
- Check released behavior when source and deployed artifact may differ.
- Record contradictions rather than smoothing them into one story.
- Prefer direct evidence to inferred intent.

## Runtime investigation

- Use temporary logging at the owning seam.
- Log typed state transitions and correlation ids.
- Avoid timing-based conclusions from a single run.
- Inject the suspected failure when safe.
- Observe progress and terminal state.
- Preserve a minimal reproducer.
- Remove temporary instrumentation before shipping.
- Turn a confirmed defect into a deterministic regression.

## External facts

- Use primary sources for technical contracts.
- Verify current docs or provider behavior when it may have changed.
- Record source date and version.
- Avoid treating marketing copy as an operational guarantee.
- Test the actual credential scope or endpoint when authorization permits.
- Do not expose secrets in probe output.
- Separate sourced fact from your inference.
- Recheck facts that gate production or release decisions.

## Stochastic systems

- One good run is a demo, not evidence of reliability.
- Define the trial population and success threshold.
- Use multiple independent trials.
- Calibrate graders against known positives and negatives.
- Report distribution, variance, and failure modes.
- Preserve representative failed traces.
- Avoid changing prompt, model, and harness simultaneously.
- Re-run after a model or provider version change.

## Kill criteria

Stop probing when:

- authoritative evidence settles the claim;
- the result cannot change the decision;
- the budget or safety boundary is reached;
- the probe is no longer representative;
- the next step requires new authority;
- repeated trials have met the predeclared threshold;
- a deterministic regression captures the behavior;
- the remaining uncertainty is explicitly accepted.

## Report

- Claim.
- Falsifier.
- Evidence and source identity.
- Result.
- Contradictory evidence.
- Confidence and limitations.
- Decision affected.
- Regression or guard added.

Do not substitute test counts for supported claims. Do not imply a product
workflow works when only an internal function was exercised.

## Verify

- Confirm the probe targets the owning interface.
- Confirm inputs and environment are recorded.
- Confirm secrets and production effects are controlled.
- Repeat unstable or stochastic results.
- Exercise the canonical path for user-facing conclusions.
- Turn durable findings into types, tests, registries, or checks.
- Remove temporary instrumentation.
- State residual uncertainty plainly.
