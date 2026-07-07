---
name: harn-probe
short: Probe-first verification — run snippets, capture outcomes as typed Facts.
description: Use to convert a guess about codebase behavior into a recorded observation by running a probe snippet and persisting the outcome as a Fact.
when_to_use: Use before asserting a claim about behavior, types, or output that you have not yet verified directly. Especially before destructive edits, before describing what a function returns, or before declaring a refactor safe.
---

# Harn probe

Use this skill when you are about to assert a claim about runtime
behavior, type-checker output, or command output that you have not
directly observed in this session. Replace the assertion with a
`probe(...)` call from `std/agent/probe`, then state the outcome as
observation, not speculation.

Pair it with [[harn-agent]] for autonomous workflows, [[harn-language]]
for the typecheck fragments you feed `probe("typecheck", ...)`, and
[[harn-testing]] for the conformance fixtures that anchor probe-derived
facts.

## Why probe instead of guess

- Guessing is cheap; verifying is structured tool use that the runtime
  can record.
- A `probe` outcome lands in the Fact ledger with
  `provenance.source = "probe"`, so a follow-up session can recall the
  verified answer instead of re-guessing.
- The cost of one probe is bounded shell or typecheck time. The cost of
  a wrong assertion is a wasted user round-trip plus a potentially
  destructive edit.
- Animals learn by *probing* — touching, tasting, testing — not by
  passive prediction. The same pattern keeps LLM coding assistants
  honest.
- Recording the *failure* outcome is just as load-bearing as recording
  the success. The ledger needs both to invalidate stale claims later.

## When to probe instead of asserting

- You are about to claim a function returns a particular value, type,
  or shape.
- You are about to declare a refactor "safe" or a migration
  "compatible".
- You are about to assert that a fragment type-checks (or fails to
  type-check) with a particular diagnostic code.
- You are about to repeat a fact from training data that may have
  drifted in this codebase.
- You are about to recommend a destructive edit (`rm`,
  `git reset --hard`, schema drop, `--no-verify`) without first
  observing that the precondition holds.
- You are about to summarize what a command prints, what a config
  defaults to, or what an env var does without having run the command
  in this session.

If you genuinely cannot probe — the surface is non-deterministic, the
body is too large, the cost is too high — say so out loud rather than
asserting.

## The probe primitive

`std/agent/probe` exposes `probe(kind, body, options?)` plus
`probe_eval` and `probe_typecheck` shorthands. Two probe kinds are
implemented today; `test` and `inspect` are reserved and currently
return `unknown`.

- `probe_eval(body, options)` runs `body` as a shell command by
  default. Set `options.lang = "harn"` to run `body` as a Harn snippet
  via `harn run`. Outcome is `pass` on exit 0 and `fail` otherwise;
  `options.expected` matches against trimmed stdout for a string and
  against the integer for a numeric comparison.
- `probe_typecheck(body, options)` writes the fragment to a temp file
  and runs `harn check <path> --json`. Outcome is `pass` when there are
  zero errors; `options.expected` matches the parsed error count.

Every probe returns a `harn.probe.v1` envelope:

- `kind`, `outcome`, `observed` — short summary of what was seen.
- `evidence` — `trace_id`, `snippet`, `command`, `stdout`, `stderr`,
  `exit_code`, `duration_ms`, plus `timed_out` when the host enforced a
  deadline.
- `fact_id` — id of the auto-stored Observation fact (pass
  `options.store_fact = false` to suppress the write).
- `expected`, `asserted_at` — round-tripped from the input.

The Fact body uses `confidence = 0.9` for observed `pass`/`fail` and
`0.4` for `unknown`, so
`recall_facts("…", "Observation", 0.8, scope)` returns only
directly-verified outcomes.

## Probe-first rules

- Probe before asserting. A claim that is not backed by a probe in this
  session or a recalled probe fact is speculation.
- Probe small. A probe that takes longer than the work you would save
  is overhead, not verification. Bound the body and pass
  `options.timeout_ms` for anything that could hang.
- Probe deterministic surfaces. Probing a live network endpoint, an
  LLM call, or wall-clock-sensitive code from inside a probe is rarely
  useful — those belong in workflows, not facts.
- Record the failure too. A probe that comes back `fail` is just as
  load-bearing as one that comes back `pass`.
- Trust the observed outcome over a recalled fact. If recall says "this
  type-checks" but a fresh probe says it does not, the fresh probe wins
  — invalidate the stale fact with `invalidate_facts` from
  `std/agent/fact` before continuing.
- Quote the trace_id, not the prose. When you reference a probe outcome
  in a user message or PR description, cite `fact_id` or
  `evidence.trace_id` so the reader can find the same observation.

## Composing with Fact recall

Probes write Observations with `provenance.source = "probe"` and
`provenance.probe_kind = "<kind>"`. Before re-running a probe, recall
with `recall_facts(query, "Observation", 0.0, scope)` to see if the
same probe already ran in a recent session. Match by
`provenance.probe_kind` to scope recall to a particular probe shape
(eval vs typecheck).

When code drift might have invalidated a recalled fact, use
`invalidate_facts({kind: "Observation", evidence: {kind: "tool_output",
ref: trace_id}}, scope)` to tombstone the stale record, then re-probe
and record the fresh outcome.

## Anti-patterns

- Probing inside a hot loop to "verify each iteration" — bound the
  loop, probe the boundary condition once.
- Probing with a body so large that the snippet evidence is meaningless
  — extract the smallest fragment that exercises the claim.
- Probing then ignoring the `outcome` field because the prose summary
  "looks right" — the structured outcome is the contract.
- Writing prose into `observed` that contradicts the actual outcome —
  keep `observed` tight and grounded in the captured stdout/stderr.
- Using `probe("test", ...)` or `probe("inspect", ...)` and expecting a
  real outcome — they are reserved and return `unknown` today; record
  the limitation and fall back to `probe_eval` with the appropriate
  test runner command.

## Probe lifecycle

1. Identify the claim you are about to assert.
2. Pick the smallest body that exercises it (a single shell command, a
   3-line Harn fragment, a single typecheck).
3. Call `probe(kind, body, options)`. Pass `options.expected` when you
   have a specific value or count in mind so the outcome is a hard
   match instead of a fuzzy success signal.
4. Inspect the returned `outcome` and `observed`. Quote
   `evidence.trace_id` or `fact_id` when summarizing to the user or in
   a PR description.
5. If the outcome contradicts a recalled fact, invalidate the stale
   fact before continuing.
6. If the outcome unblocks the original task, proceed. If it surfaces
   an unexpected failure, treat that as new information and decide
   whether to fix, escalate, or abandon — do not re-assert the original
   guess.

## Example probes

```harn
import { probe_eval, probe_typecheck } from "std/agent/probe"

// 1. Verify a shell helper before relying on its exit code.
const helper = probe_eval(
  "git diff --quiet HEAD -- crates/harn-stdlib",
  {expected: 0, timeout_ms: 5000},
)
if helper.outcome != "pass" {
  __io_println("stdlib has uncommitted edits — recall fact: " + helper.fact_id)
}

// 2. Confirm a fragment type-checks before pasting it into the user's file.
const tc = probe_typecheck(
  "pipeline summary() { const x: int = len([1, 2, 3]) __io_println(x) }\n",
  {expected: 0},
)
require tc.outcome == "pass", "fragment fails typecheck: " + tc.observed
```

## Verify

- Stdlib catalog: `cargo test -p harn-stdlib --test stdlib_modules`
  round-trips the source.
- Conformance:
  `cargo run --quiet --bin harn -- test conformance --filter agent_probe`.
- Manual smoke: `harn run -e 'import {probe_eval} from "std/agent/probe";
  __io_println(probe_eval("echo hi").outcome)'`.
- Skill body: `cargo test -p harn-skills` covers the embedded corpus
  invariants.

## Kill criterion

If probe-first injection slows tasks by more than 50 percent without
measurably reducing wrong-assertion incidents over 20 sessions,
restrict probing to high-risk operations (before destructive edits,
before declaring a refactor safe) or abandon the always-on nudge.
Document the kill decision as a Decision fact so future sessions
inherit the rationale.
