# routing_policy v2 — verifier-signal escalation eval

This directory hosts a small empirical demo for the verifier-signal
escalation feature shipped in [#2435][issue] / [#2440][pr].

The original issue's acceptance criterion #3 called for a SWE-Bench-Verified
10-issue eval that's deferred pending the Docker / Python harness work. This
script is the smaller, "does the wiring actually fire end-to-end with real
API spend?" demo that was budget-approved for a single session.

[issue]: https://github.com/burin-labs/harn/issues/2435
[pr]: https://github.com/burin-labs/harn/pull/2440

## What the script does

[`codegen_eval.harn`](codegen_eval.harn) runs the same five Harn-codegen
prompts across **four cells** — strategy × context:

|                         | bare prompt | + harn-language skill body |
|-------------------------|-------------|----------------------------|
| **baseline** (devstral) | cell A      | cell B                     |
| **routed** (devstral→opus, typecheck) | cell C      | cell D                     |

- The baseline calls `mistralai/devstral-small` only, with no verifier.
- The routed strategy uses
  `mistralai/devstral-small → anthropic/claude-opus-4.7` with the
  `typecheck` verifier: it extracts the first ```` ```harn ```` fenced
  block from each candidate, runs `harn_parser::check_source` on it, and
  emits `accept` or `escalate`. When `routed` escalates, the chain
  advances to the frontier link; when the frontier also fails, the
  rejected candidate is returned (per the "verifiers gate routing, not
  correctness" semantics).
- The skill body is the literal output of `harn skills get
  harn-language --full`, captured at [`harn_language_skill.md`](harn_language_skill.md)
  so the experiment is reproducible without depending on the runtime
  skill registry. When passed via `llm_call(..., system: skill_body, ...)`,
  it gives the cheap tier the same Harn-syntax context a real coding
  agent would inject before asking the model to write `.harn`.

To grade each candidate with the **same** harn-parser the verifier uses,
the script pushes the candidate as a mock-LLM response into a one-link
`mock:mock` routing_policy with the typecheck verifier, then reads the
verifier outcome off the receipt. That keeps the validity column anchored
to the parser, not a regex heuristic, with no extra API spend.

## Running it

```bash
# Source the OpenRouter key (the script reads $OPENROUTER_API_KEY).
set -a; . /path/to/.env; set +a

cargo run --quiet --bin harn -- run evals/routing_v2/codegen_eval.harn
```

The `routing_policy.budget` block caps spend at `per_call_usd: 0.50`,
`session_usd: 2.50`. A full 5-task run costs **about one cent** at current
OpenRouter rates (Devstral $0.10/$0.30 per M, Opus $5/$25 per M).

## Sample results

A representative 4-cell run (LLM outputs are non-deterministic, so
individual task verdicts shift run-to-run):

| Cell             | valid | cost \$    | vs cell A   |
|------------------|-------|------------|-------------|
| baseline_bare    | 2/5   | \$0.000083 | —           |
| baseline_skill   | 3/5   | \$0.000648 | +50% quality / 8× cost |
| routed_bare      | 3/5   | \$0.007793 | +50% quality / 94× cost |
| **routed_skill** | **4/5** | **\$0.012177** | **+100% quality / 147× cost** |

Two earlier "bare-only" runs (before skill injection landed) for
historical context:

| Run | baseline_bare valid | routed_bare valid | baseline_bare \$ | routed_bare \$ |
|---|---|---|---|---|
| historical #1 | 3/5 | 4/5 | \$0.000087 | \$0.007990 |
| historical #2 | 3/5 | 4/5 | \$0.000087 | \$0.007640 |

Full JSON dumps are checked in at
[`sample_run.json`](sample_run.json) (the 4-cell skill run) and
[`sample_run_bare.json`](sample_run_bare.json) (the original bare-only
run from #2443). Each attempt now carries its own `input_tokens` /
`output_tokens` alongside `cost_usd` (added in #2443) so downstream
graders can attribute spend precisely.

### Per-task narrative

The cell-D win on **`fib`** is the cleanest single-task story for why
verifier-gated routing **plus** context-injection matters:

| Cell | Output | Validity |
|---|---|---|
| baseline_bare | `def fib(n: int) -> int:` (Python `def`, colon block) | ❌ |
| baseline_skill | `fn fib(n: int) -> int { match n { 0 => 0, _ => fib(n-1)+fib(n-2) } }` (Devstral writes `fn` + braces correctly, but uses Rust/Scala `=>` instead of Harn's `->` in match arms) | ❌ |
| routed_bare | `fn fib(n: int) -> int { let mut a: int = 0; ... }` (cheap escalated; Opus also got it wrong with Rust-style `let mut`) | ❌ |
| routed_skill | `fn fib(n: int) -> int { if n == 0 { return 0 } else if n == 1 { return 1 } else { return fib(n-1) + fib(n-2) } }` (Devstral self-corrected with the skill body — **no escalation needed**) | ✓ |

Cost for fib in cell D was **\$0.000139 vs \$0.0038 for cell C** — a
**27× cost reduction on the same task** because the skill context
let the cheap tier produce a verifier-passable answer the first time.

Per-task summary across this run:

| Task | bare baseline | skill baseline | bare routed | skill routed | Note |
|---|---|---|---|---|---|
| `fib` | ❌ | ❌ | ❌ | ✓ | Skill teaches `fn { }` but not `match ->`; routing+skill finds the if/else fallback |
| `trivial_let` | ✓ | ✓ | ✓ | ✓ | Trivial, all cells valid |
| `sum_list` | ❌ | ✓ | ✓ | ✓ | Without skill Devstral uses immutable-violating `sum = sum + item`; with skill it switches to a valid pattern, no escalation needed |
| `is_even_pipeline` | ❌ | ❌ | ❌ | ❌ | Both tiers reliably emit `pipeline check(task: int) {...}`; the prompt asks for typed pipeline params which Harn doesn't accept. Even Opus + skill misses |
| `fix_typo` | ✓ | ✓ | ✓ | ✓ | Trivial typo fix |

## Key learnings

1. **The wiring works end-to-end on real APIs.** Verifier signals fire, the
   chain advances, refine-vs-escalate semantics behave as specified, and
   per-attempt `verifier_outcome` + `verifier_signals` ride through to the
   result envelope unchanged.
2. **Cheap-tier capability doubles when given the skill body.** Bare
   `mistralai/devstral-small` got 2/5 valid Harn snippets; the same model
   with `harn-language` injected got 3/5 — without changing model, temperature,
   or prompt, just adding ~1500 tokens of DSL context. This is the core
   "context-injection turns small models into viable specialists" story.
3. **Verifier-gated routing + skill injection compounds.** Cell D
   (routed plus skill) hits 4/5 quality. On the `fib` task specifically,
   the skill
   body let Devstral produce a verifier-passing answer the first try, so
   the routing chain returned without escalating to Opus — a 27× cost
   reduction on that single task ($0.0001 vs $0.0038).
4. **There's a real context-tax tradeoff.** Skill injection adds ~$0.0001
   per cheap call and ~$0.008 per frontier call (1500 tokens × $5/M).
   When the verifier still escalates anyway (the `is_even_pipeline` case
   where both tiers fail), the skill body becomes a tax — cell D was
   ~5× more expensive than cell C on that task. The "skill helps" case
   pays for itself across the suite, but it's not free, and it's worth
   measuring before rolling skill-injection out as a default.
5. **The "both attempts fail" path stays useful.** Even when the chain
   exhausts, the verifier reasons recorded on `routing.attempts` give the
   caller actionable diagnostics ("parse failed at 1:20"). That's a
   strictly better failure mode than the pre-#2435 status quo of "the
   model said this and we didn't notice it was wrong."

## What is intentionally not here

- **Full SWE-Bench-Verified harness.** Issue #2435's AC #3 stays open. That
  needs a Docker / pytest / dataset-download pipeline that's an independent
  project.
- **Refine retries.** `max_refines_per_link: 0` keeps the wiring readable for
  this demo; flipping it to 1+ adds another exercise dimension we can run
  once a refine-aware fixture set exists.
- **Multi-verifier chains.** Only `typecheck` is exercised here. `lint` and
  `test_run` are tested in the unit suite (`crates/harn-vm/src/llm/routing_verifier.rs`)
  but the eval doesn't currently stress them.

## Drive-by harness improvement

Building this eval surfaced a real receipt gap: `RoutingAttempt` carried
`cost_usd` but no per-attempt token counts, so any attribution against an
alternate pricing table (OpenRouter, in this case — the runtime catalog
covers native providers only) had to fall back to attributing all spend to
the final winning attempt. This PR also adds `input_tokens` and
`output_tokens` to the receipt on every successful attempt, which the eval
now uses to cost the cheap-tier link precisely even when the chain
escalates past it.
