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
prompts through two strategies side by side:

| Strategy | Chain | Verifier |
|---|---|---|
| `baseline` | `mistralai/devstral-small` only | none |
| `routed`   | `mistralai/devstral-small → anthropic/claude-opus-4.7` | `typecheck` (harn-parser) |

The verifier extracts the first ```` ```harn ```` fenced block from each
candidate, runs `harn_parser::check_source` on it, and emits `accept` or
`escalate`. When `routed` escalates, the chain advances to the frontier link;
when the frontier also fails, the rejected candidate is returned (per the
"verifiers gate routing, not correctness" semantics).

To grade the baseline output with the **same** harn-parser the verifier uses,
the script pushes each candidate as a mock-LLM response into a one-link
`mock:mock` routing_policy with the typecheck verifier, then reads the
verifier outcome off the receipt. That keeps the validity column anchored to
the parser, not a regex heuristic, with no extra API spend.

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

Four independent runs collected during initial validation (LLM outputs are
non-deterministic, so individual task verdicts shift run-to-run):

| Run | baseline valid | routed valid | baseline $ | routed $ | escalations |
|---|---|---|---|---|---|
| 1 | 3/5 | 4/5 | $0.000087 | $0.007990 | 2/5 |
| 2 | 3/5 | 4/5 | $0.000087 | $0.007840 | 2/5 |
| 3 | 3/5 | 4/5 | $0.000087 | $0.007640 | 2/5 |
| 4 | 3/5 | 3/5 | $0.000087 | $0.006037 | 2/5 |

A representative full JSON dump (run #4) is checked in at
[`sample_run.json`](sample_run.json). Notice that each attempt now carries
its own `input_tokens` / `output_tokens` alongside `cost_usd` — added in
this PR so downstream graders (this eval included) can attribute spend
against arbitrary pricing tables, not just the runtime catalog.

### Per-task narrative

| Task | What happened (typical) |
|---|---|
| `fib` | Devstral emits Python-style `function fib(n: int) -> int:` (colon-block, `function` keyword). Verifier flags at 1:5–1:10. Opus *usually* produces canonical `fn fib(n: int) -> int { ... }` and passes; in run #4 Opus instead reached for Rust-style `let mut a: int = 0` and the verifier flagged that too. **Routing kept escalating but the chain exhausted, so the receipt records both rejections and returns Opus's candidate — strictly more diagnostics than a single-tier call would have surfaced.** |
| `trivial_let` | Both models emit `let answer: int = 42`. Verifier accepts. No escalation, no cost overhead. |
| `sum_list` | Both models produce parseable Harn `fn sum_list`. Verifier accepts. No escalation. (The bodies are semantically sketchy — neither model handles Harn's by-value mutation rules — but that's beyond the typecheck verifier's reach.) |
| `is_even_pipeline` | Devstral **and** Opus reliably both emit `pipeline check(task: int) {...}` — `pipeline` parameters don't accept type annotations like `fn` does. Verifier flags both at 1:20. Routing exhausts the chain and returns the frontier candidate; receipt shows `verifier_outcome: escalate` on both attempts so a downstream pass can react. |
| `fix_typo` | Both models correct `funtion` → `fn`. Verifier accepts. No escalation. |

## Key learnings

1. **The wiring works end-to-end on real APIs.** Verifier signals fire, the
   chain advances, refine-vs-escalate semantics behave as specified, and
   per-attempt `verifier_outcome` + `verifier_signals` ride through to the
   result envelope unchanged.
2. **Cost overhead is real but absolute spend is tiny.** Routed is ~90× more
   expensive than the bare cheap-tier baseline on this suite, but the total
   for five tasks is still under one cent. With a higher base rate of
   verifier-passing answers (most production prompts), the ratio collapses
   toward 1×.
3. **The "both attempts fail" path is more useful than it looks.** Even when
   the chain exhausts, the verifier reasons recorded on `routing.attempts`
   give the caller actionable diagnostics ("parse failed at 1:20"). That's a
   strictly better failure mode than the pre-#2435 status quo of "the model
   said this and we didn't notice it was wrong."

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
