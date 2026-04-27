# Flow Predicate Language

Status: design decision record for Harn Flow v0. Parent issue:
[harn#571](https://github.com/burin-labs/harn/issues/571). This page
closes the four predicate-language review questions in
[harn#584](https://github.com/burin-labs/harn/issues/584) and records the
implementation work that is already landed or in flight.

Flow predicates are repo-local Harn functions that gate candidate slices. They
are not a separate DSL. A repository declares them in per-directory
`invariants.harn` files, and Flow discovers them with the same root-to-leaf
shape used by directory metadata. The goal is a policy surface that can be
audited, replayed, and proposed by agents without letting agents silently expand
their own authority.

## Current Implementation State

The design in this document assumes the following ticket state as of
2026-04-26:

| Area | Status |
|---|---|
| Predicate executor and hard kind budgets | Landed in #578 / #704. |
| Cross-slice fairness scheduler and aggregate budget envelopes | Landed in #736. |
| `invariants.harn` discovery and attributes | Landed in #579. |
| `InvariantResult`, evidence, and remediation types | Landed in #581. |
| Hierarchical predicate composition | In review in #731, closing #582. |
| Predicate hash replay audit | In review in #730, closing #583. |
| Archivist persona | Landed in #586 as deterministic propose-only scan output. |
| Fixer persona | In review in #729, closing #587. |

## Predicate Declarations

Every shipping predicate is a top-level Harn function marked with
`@invariant` and exactly one execution kind:

```harn
@invariant
@deterministic
@archivist(
  evidence: ["https://example.com/team/security-rule"],
  confidence: 0.94,
  source_date: "2026-04-26",
  coverage_examples: ["crates/api/src/auth.rs"]
)
fn no_raw_tokens(slice) {
  return flow_invariant_allow()
}

@invariant
@semantic(fallback: no_raw_tokens)
@archivist(
  evidence: ["https://example.com/team/security-rule"],
  confidence: 0.84,
  source_date: "2026-04-26",
  coverage_examples: ["crates/api/src/auth.rs"]
)
fn no_raw_tokens_semantic_review(slice) {
  return flow_invariant_warn("semantic review found risky token-like text")
}
```

`@deterministic` predicates are pure Harn. They cannot use the network, shell,
LLM calls, host tools, clocks, random sources, or mutable ambient state.

`@semantic(fallback: name)` predicates may make one cheap judge call over
pre-baked evidence captured in `@archivist(...)`. The fallback must name a
deterministic predicate declared in the same `invariants.harn` file or an
ancestor file. They still cannot fetch fresh evidence during slice evaluation.

The result is an `InvariantResult`:

```harn
flow_invariant_allow()
flow_invariant_warn("needs cleanup soon")
flow_invariant_block("secret_leak", "raw token appears in the diff")
flow_invariant_require_approval("role", "security")
```

Evidence items point at atoms, metadata paths, transcript spans, or external
citations. Remediation is inert: it is input to Fixer, never an auto-apply
instruction.

## Decision 1: Budget Semantics Under Concurrency

Default stance: per-slice budget envelopes with a fairness scheduler.

Per-predicate global budgets are too easy to game. A slice can split one costly
semantic check into many tiny predicates and consume the same shared resources
while looking compliant. Per-slice serial budgets avoid that but create
head-of-line blocking: one semantic-heavy slice can delay small deterministic
slices behind it.

The v0 rule is:

- Each predicate keeps a hard local timeout by kind: deterministic predicates
  use the existing 50 ms CPU target, and semantic predicates use the existing
  2 s wall-clock target with one cheap judge call and a token cap.
- Each candidate slice also gets one aggregate evaluation envelope covering all
  predicates selected for that slice.
- The scheduler admits work fairly across slices, not just within one slice.
  When multiple slices are queued, no slice may occupy all semantic lanes, and
  deterministic work for later slices must continue to make progress while
  earlier semantic work is waiting.
- Budget exhaustion is a structured `Block` with code `budget_exceeded`, never
  a panic and never an implicit approval.

The simple implementation target is weighted round-robin across active slices:
run deterministic predicates first, then semantic predicates with a small
bounded semantic lane count. Within a slice, preserve deterministic output order
by sorting records after execution, as the current executor already does.

This keeps the mental model dumb: predicates are still ordinary Harn functions,
but admission control is owned by the Flow scheduler instead of by each
predicate.

### Implementation

The scheduler lives on
[`PredicateExecutor`](https://docs.rs/harn-vm/latest/harn_vm/flow/struct.PredicateExecutor.html)
and is configured via `PredicateSchedulerConfig`:

- `max_deterministic_lanes` / `max_semantic_lanes` — global concurrency caps
  shared across every queued slice. Deterministic and semantic lanes are
  independent semaphores, which is what makes deterministic work continue to
  make progress while semantic work is queued behind a scarce cheap-judge
  budget.
- `max_deterministic_lanes_per_slice` / `max_semantic_lanes_per_slice` —
  per-slice caps that prevent any single slice from occupying every global
  lane. The default semantic-per-slice cap of 1 plus FIFO permit ordering is
  enough to interleave queued slices fairly under the typical 2-lane semantic
  budget.
- `slice_deterministic_envelope` / `slice_semantic_envelope` — aggregate
  per-kind wall-clock envelopes for one slice. Once exhausted, every
  remaining predicate of that kind for that slice short-circuits to a
  structured `Block { error: { code: "budget_exceeded" } }` instead of
  silently allowing it through.

Use `PredicateExecutor::execute_slices(slices)` when more than one candidate
slice is queued together. The single-slice convenience method
`execute_slice(slice, predicates)` still works and routes through the same
scheduler so per-slice envelopes apply uniformly.

## Decision 2: Bootstrap Signing

Default stance: add a minimal, hand-authored root `meta-invariants.harn` that
governs predicate authorship. Archivist may propose edits to `invariants.harn`,
but Archivist may not author or auto-promote the root bootstrap policy.

`meta-invariants.harn` is intentionally smaller than normal predicate files. It
answers only "what predicate changes are acceptable for review?" and must not
become a second application policy layer.

The required bootstrap checks are:

- Predicate files must be valid Harn and must use `@invariant` plus exactly one
  of `@deterministic` or `@semantic`.
- Every non-bootstrap predicate must carry `@archivist(...)` provenance with
  evidence, confidence, source date, and coverage examples.
- `@semantic` predicates must name a deterministic fallback predicate in the
  same file or an ancestor file.
- External citations must be fetched at authoring time and pinned in the
  predicate metadata; evaluation-time network fetches are forbidden.
- Predicate edits proposed by Archivist remain propose-only. Promotion requires
  a human approval signature in the slice approval chain.
- Edits to `meta-invariants.harn` require human maintainer approval and are
  validated against the previous committed bootstrap policy hash. The initial
  root file is seeded by a human-reviewed commit.

This is similar in spirit to supply-chain attestation systems such as
in-toto/SLSA: the policy code, the subject it evaluated, and the actor that
approved it must be separable audit facts. Harn's subject is a Flow slice rather
than a build artifact, but the trust boundary is the same.

### `meta-invariants.harn` shape and validators

The bootstrap file is parsed by the existing Harn lexer/parser — the only
syntactic novelty is a top-level `@bootstrap_maintainers(approvers: [...])`
attribute that lists human maintainer roles or principals allowed to promote
bootstrap edits. A maintainer entry is `"role:<name>"` (matched as
`Approver::Role`) or `"user:<id>"` / any other string (matched as
`Approver::Principal`). Repos that omit the attribute fall back to a single
`role:flow-platform` approver — the same default used by the Ship Captain
predicate-count ceiling.

Two Rust functions in
`crates/harn-vm/src/flow/predicates/bootstrap.rs` carry the policy:

- `validate_predicate_edit(proposed_source, author, previous_policy)` runs
  the bootstrap rules against a proposed `invariants.harn` edit. Missing
  `@archivist(...)` provenance, partial provenance fields, kind-collision
  attributes, and unresolved semantic fallbacks are all promoted from the
  parser's soft warnings into hard `Block` verdicts with stable codes
  (`bootstrap_missing_archivist`, `bootstrap_archivist_provenance_incomplete`,
  `bootstrap_kind_collision`, `bootstrap_missing_semantic_fallback`,
  `bootstrap_unresolved_semantic_fallback`). The previous bootstrap policy
  hash is pinned in the validation result so the slice approval chain has an
  explicit audit pointer.
- `validate_bootstrap_edit(proposed_source, author, previous_policy)` runs
  against a proposed `meta-invariants.harn` edit. Archivist authorship is a
  hard `Block` (`bootstrap_archivist_cannot_author_bootstrap`); any
  non-Archivist actor returns `RequireApproval` routed to one of the
  previous policy's maintainers (or the default role on initial seed). The
  previous policy hash and the proposed policy hash are both pinned in the
  result.

`harn flow ship watch` and `harn flow archivist scan` now surface the
discovered bootstrap policy alongside the existing predicate validation
payload. When `meta-invariants.harn` is present at the predicate root, the
JSON output includes `bootstrap_policy.status = "present"` together with the
policy hash and the resolved maintainer list; otherwise the field reports
`status = "absent"` and the path Flow looked for. The actual `Block` /
`RequireApproval` verdicts from the validators are returned to in-process
callers (Archivist promotion, future `harn flow validate` subcommand) — Ship
Captain only surfaces the discovered policy hash and maintainers, since
Phase 0 doesn't yet evaluate proposed edits during atom emission.

## Decision 3: Semantic Predicate Determinism

Default stance: every `@semantic` predicate must have a deterministic fallback.

Pinned model identifiers and temperature zero are useful audit metadata, but
they are not a replay guarantee. Provider behavior, model patch versions,
safety filters, and context packaging can drift. Treating semantic predicates
as inherently non-replayable is honest but too weak for a shipping gate.

The v0 rule is:

- `@semantic` predicates may influence a current slice only when they declare a
  deterministic fallback.
- The fallback must be evaluated and recorded in `invariants_applied` alongside
  the semantic predicate.
- If the semantic predicate and fallback disagree, the stricter verdict wins:
  `Block` over `RequireApproval` over `Warn` over `Allow`.
- Replay audits use the pinned predicate source hashes. Semantic result drift is
  advisory unless the deterministic fallback also fails.
- Predicate hashes include the predicate source. For semantic predicates, audit
  records should additionally retain model id, provider id, prompt hash,
  evidence hashes, token cap, and cheap judge version.

This keeps semantic checks useful for judgement-heavy review while making the
replay path depend on deterministic code. It also aligns with policy engines
such as CEL and OPA: fast deterministic checks should carry the enforceable
contract, while richer evaluators can annotate and escalate.

## Decision 4: Cross-Directory Slice Composition

Default stance: use the union of all predicates applicable to every touched
directory, with de-duplication for shared ancestors and explicit explosion
limits.

Intersection is unsafe. If a slice touches `docs/` and `crates/harn-vm/`, the
predicate set common to both directories may exclude the VM-specific invariant
that actually matters. Union is stricter and matches the semantics users expect:
touching a directory means accepting that directory's rules.

The v0 rule is:

- For each touched directory, collect root-to-leaf `invariants.harn` files.
- Union the resulting predicate declarations across touched directories.
- De-duplicate shared ancestors by `(source_dir, predicate_name)`.
- Keep same-named predicates in sibling directories independent.
- Compose ancestor and child results by strictness. A child may tighten an
  ancestor, but it cannot relax a shallower `Block`; equal strictness keeps the
  shallower predicate canonical.
- Enforce a predicate-count ceiling before evaluation. If a slice exceeds the
  ceiling, Flow returns a structured `RequireApproval` or `Block` explaining
  the predicate explosion instead of silently skipping rules.

### Default ceiling

The `PredicateCeiling::default()` in `crates/harn-vm/src/flow/predicates/compose.rs`
sets:

- `require_approval_threshold = 256` — at this size the `flow-platform` role
  is asked to co-sign before the slice ships.
- `block_threshold = 1024` — at this size Flow refuses the slice with the
  stable error code `predicate_count_explosion`.

These limits are operational, not perf. The
`crates/harn-vm/benches/flow_predicate_union.rs` benchmarks measure resolve and
ceiling-check cost across normal, high-fanout, and pathological fixtures: the
union itself is microsecond-scale even at ~2000 predicates and the ceiling
check is sub-100µs. The binding constraint is downstream evaluation —
deterministic predicates carry a 50ms wall-clock budget each, so a 256-predicate
slice can spend 13s of serial work before Ship Captain even renders results.
The ceiling makes that cost visible to a human before it becomes load-bearing.

The structured violation surfaces:

- `count` and `threshold` so operators can see how far over budget the slice is.
- Up to five `top_contributors`, each `{ relative_dir, count }`, so it is
  obvious which directory's `invariants.harn` is fanning out.
- `level` of `require_approval` or `block`.

`harn flow ship watch` already routes the violation into its
`predicate_validation.ceiling` payload and propagates the level into
`mock_pr.validation_status`.

The open implementation work is exhausted. Ship Captain may evaluate
cross-directory unions without a human in the loop only because the ceiling
makes the cost visible and bounded.

## Replay And Audit Contract

Every shipped slice records every predicate hash and result that gated it. A
later predicate change cannot retroactively unship historical work.

Replay audit is advisory by default:

- A slice is replayed against current `@retroactive` predicates.
- Drift is reported with current predicate hashes and historical-only hashes.
- `harn flow replay-audit --fail-on-drift` may turn advisory drift into a CI
  failure for repositories that want that policy.
- Historical slices are never rewritten by replay.

This matches the append-only Flow model: new facts create new atoms, slices, or
audit records; they do not mutate old shipping decisions.

## Archivist Proposal Scans

Archivist v0 is intentionally dumb and review-first. It does not promote
predicates and it does not fetch live evidence during slice evaluation. The CLI
entrypoint inventories a repository, loads the Flow persona manifest when it is
present, mines local convention and motion signals, and emits proposal records
with concrete Harn predicate source:

```bash
harn flow archivist scan . --json
harn flow archivist scan . --manifest examples/personas/flow.harn.toml \
  --store .harn/flow.sqlite --shadow-days 30 --out .harn/archivist/proposals.json
```

The JSON payload contains:

- `manifest`: whether the Archivist persona manifest loaded and which
  `[[personas]]` entry was used.
- `inventory`: detected stacks, lockfiles, config files, and source roots.
- `convention_signals`: lint/config files and inline `invariant:` comments.
- `motion_signals`: recent git-log buckets such as tests, formatting, Flow
  predicates, and release docs.
- `existing_predicates`: discovered `invariants.harn` predicates and discovery
  diagnostics.
- `proposals`: review-ready `@invariant` + `@archivist(...)` predicate source,
  evidence URLs, confidence, source date, coverage examples, and a permanent
  `propose_only` autonomy marker.
- `shadow_evaluation`: best-effort coverage against recent Flow atoms in the
  local SQLite store, including false-positive candidates with atom ids,
  transcript refs, and diff spans.

If no Flow store exists, `shadow_evaluation.status` is `no_flow_store` rather
than an error. That keeps initial repo bootstrap useful while making the
absence of atom history explicit.

## Follow-Up Implementation Tickets

The decisions above leave concrete implementation work beyond the already
landed and in-review predicate tickets:

- ~~[#734](https://github.com/burin-labs/harn/issues/734): add
  `meta-invariants.harn` bootstrap validation and approval-chain checks.~~
  Closed by `validate_predicate_edit` /
  `validate_bootstrap_edit` plus the `bootstrap_policy` field surfaced in
  `harn flow ship watch` and `harn flow archivist scan`.
- [#735](https://github.com/burin-labs/harn/issues/735): deterministic
  fallback metadata and enforcement for `@semantic` predicates.
- ~~[#736](https://github.com/burin-labs/harn/issues/736): add cross-slice
  fair scheduling and aggregate per-slice predicate budget envelopes.~~
  Closed by `PredicateSchedulerConfig` and `execute_slices(slices)` documented
  above.
- ~~[#733](https://github.com/burin-labs/harn/issues/733): add
  cross-directory union benchmarks and predicate-count explosion limits before
  Ship Captain relies on unattended slice emission.~~ Closed by the
  `PredicateCeiling` defaults documented above plus the
  `flow_predicate_union` criterion bench.

These are implementation follow-ups to #584, not new design questions.

## External Reference Points

The design intentionally stays close to proven policy and provenance shapes:

- CEL emphasizes fast, safe, host-data-only policy evaluation:
  <https://cel.dev/>.
- OPA treats side-effect-free policy evaluation as the normal case and requires
  care around I/O builtins:
  <https://www.openpolicyagent.org/docs/extensions>.
- in-toto and SLSA use attestations that bind subjects, predicates, and
  provenance:
  <https://slsa.dev/blog/2023/05/in-toto-and-slsa>.
