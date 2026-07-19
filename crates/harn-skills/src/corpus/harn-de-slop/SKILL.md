---
name: harn-de-slop
short: Find and remove duplicated policy, weak contracts, and low-value test machinery.
description: Use for maintainability audits and refactors across Harn code, especially duplicated orchestration, stringly or open dict contracts, silent fallbacks, oversized modules, repeated test harnesses, flaky waits, and hand-maintained tables that should have one typed owner.
when_to_use: Use before or during a de-slop pass, cross-module refactor, type-safety cleanup, test-harness consolidation, or architecture review where the goal is less code with stronger contracts.
---

# Harn de-slop

Remove the highest-cost duplication at its owning boundary. Prefer a smaller
typed system with an empirical guard over a broad style cleanup.

Pair with [[harn-language]] for type and module contracts, [[harn-testing]] for
test altitude, and [[harn-diagnostics]] when a defect first appears as a vague
runtime failure.

## Establish the measured problem

- Read the repository instructions and ownership map.
- Inspect recent issues, PRs, and commits around the suspected seam.
- Trace the real public entrypoint through its executor; distinguish the
  canonical product path from alternate, test-only, and dead engines.
- Search every spelling and caller before choosing an owner.
- Count duplicated implementations, fields, aliases, and lines.
- Separate identical defaults from behavior-specific overrides.
- Record one concrete failure or change-amplification example.
- Claim the slice before editing when other agents may be active.

Do not start with formatting, naming, or a preferred abstraction. Start with a
measured maintenance or correctness cost.

Useful inventory commands:

```text
rg -n '<constructor|fallback|builtin|helper>' <roots>
rg -l '<pattern>' <roots> | wc -l
git log --all --oneline -- <paths>
git blame -L <start>,<end> <file>
```

## Choose the owner

Put one contract in the narrowest dependency-leaf layer that owns the shape or
policy. Check the crate/module dependency direction before editing; a shared
contract must not create an inverse parser, IR, VM, host, or product dependency.

- Harn owns orchestration, retries, transcript semantics, replay, and context.
- A native host primitive owns OS, process, storage, or transport facts.
- Product code owns editor facts, approval UX, and product persistence.
- A test support module owns hermetic fixture defaults shared by test binaries.
- Generated artifacts derive from one schema; do not hand-maintain mirrors.

Use the change-amplifier test: if adding one required field forces dozens of
unrelated edits, those callers are constructing an owner type too directly.

Move classification upstream when a consumer would otherwise inspect prose,
guess from substrings, or revalidate a producer guarantee. Fix the producer's
type when callers cannot safely lean on the current contract.

## Prefer closed contracts

- Replace open `{ok, value?, error?}` bags with `Result<T, E>`.
- Use a closed literal union for finite failure kinds.
- Give records exact required and optional fields.
- Use `unknown` at an untrusted generic boundary; narrow it once downstream.
- Use nominal specs for atomic multi-field operations.
- Keep one default in the owning constructor or normalizer.
- Remove caller-side `?? default` when the producer already defaults.
- Preserve valid generic values; enforce product-specific shapes downstream.
- Carry raw detail beside a stable typed kind for diagnostics.
- Make required reads fail loudly; model optional absence explicitly.

Use a direct cutover by default: delete the old API, aliases, tests, and docs in
the same migration. Retain dual support only for an explicit versioned external
contract with an owner, removal condition, and structural removal gate.

## Delete duplication safely

1. Write the canonical helper or contract with explicit defaults.
2. Migrate one representative simple caller.
3. Migrate callers with real overrides and keep those overrides visible.
4. Delete redundant imports, passthrough blocks, and default reassignments.
5. Recount the old pattern and inspect every survivor.
6. Add a structural guard that rejects recurrence.
7. Trace the public entrypoint again and prove the old path is unreachable.

Prefer syntax-aware guards over substring scans. A guard must include a
falsifier that proves it catches alternate formatting or qualification.

Do not consolidate distinct semantics because two functions happen to look
similar. Name the invariant shared by every migrated caller.

Keep prose in one Diataxis owner: procedures in how-to guides, contracts in
reference, learning paths in tutorials, and rationale in explanation. Remove
incident archaeology and repeated implementation narration from user docs.

## Put tests at the right altitude

- Test a pure normalizer with table-driven unit cases.
- Test a boundary contract with a real parser, filesystem, or protocol fixture.
- Test orchestration with deterministic Harn and mocked capabilities.
- Test CLI integration through the canonical warmed process harness.
- Use live providers only when provider behavior is the subject.
- Prefer structured outcomes over rendered prose assertions.
- Include a pre-fix falsifier for correctness bugs.
- Include an ambiguity control when multiple causes can look identical.
- Avoid sleeps, reset-on-output deadlines, and substring-only success checks.

For filesystem and parser defects, create empirical fixtures for success,
absence, permission failure, malformed input, and exact resource limits. Model
permission failure through a capability or sandbox denial so it is portable;
do not rely on `chmod` semantics. Do not infer a size or timeout cause from one
failing artifact.

## Split long files by ownership

Treat a hand-maintained source file above 1,500 lines as decomposition debt.
Do not grow its existing baseline. If the task touches that owner, extract
at least one cohesive behavior and lower the recorded count; generated files
remain generator-owned and are exempt from manual splitting.

Split when a file contains multiple independently testable policies or
registration families. Keep together code that shares private invariants and
would require a new pass-through module merely to shorten the file.

After a split:

- preserve one public entry point;
- keep dependency direction acyclic;
- name modules by behavior, never `part1`, `misc`, or `utils`;
- move tests beside their owner where local convention permits;
- delete re-export aliases that add no boundary;
- check generated tables and documentation for drift.

## Review the reduction

Review the final diff from scratch, not as a sequence of edits.

- Compare every exceptional old value with the new override.
- Look for defaults now applied too broadly.
- Look for a new helper that merely hides a large parameter bag.
- Check whether the guard has become a second parser or policy owner.
- Check exact argument, result, effect, and lifecycle types.
- Check error attribution and observability at failure boundaries.
- Recount lines and duplicated sites, but do not treat LOC alone as correctness.
- Rebase on current main and repeat the semantic diff review.

## Verify and ship

Run the narrowest contract test first, then the repository gates appropriate to
the changed surface. Use isolated target directories and respect shared
hardware leases.

Before publishing:

- format and lint changed Harn files;
- run focused conformance or crate tests;
- run generated-artifact drift checks when applicable;
- run `git diff --check`;
- verify the old pattern is absent or explicitly allowlisted;
- use a signed commit;
- enable auto-merge only after the latest-main rebase;
- shepherd CI to a terminal result and root-cause failures.

Report the typed contract, empirical authority, net deletion, structural guard,
and any intentionally deferred debt. Do not claim convergence from an N=1 run
or a qualitative transcript read.
