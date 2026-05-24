# Step-Judge Experiment — Report (2026-05-23)

**Decision: GO** as opt-in. Ship `step_judge` with `on_veto: "replace"` and
the neutral rubric as defaults (both empirically validated). Document the
asymmetric pairing (cheap generator + strong judge) as the recommended
preset; do not recommend symmetric pairings.

## Headline

| Cell | Pass rate | Cost (USD) | Lift vs baseline |
|---|---:|---:|---:|
| `baseline-cheap` (Haiku 4.5 alone) | 50% | $0.31 | — |
| `symmetric-cheap` (Haiku + Haiku judge) | 50% | $0.29 | 0pp |
| `asymmetric` (Haiku + Sonnet judge) | **83%** | **$0.14** | **+33pp** |
| `symmetric-strong` (Sonnet + Sonnet) | 83% | $0.33 | +33pp |

Directional probes vs `asymmetric` default: `adversarial rubric` −33pp,
`retain on_veto` −33pp. Both shipped defaults validated.

6 fixtures × 1 replicate per cell, OpenRouter, `--tool-format text`,
total spend $1.45.

## Per-fixture truth table

(✓ = passed, ✗ = failed)

| Fixture | baseline | sym-cheap | asym | sym-strong | adv | retain |
|---|:---:|:---:|:---:|:---:|:---:|:---:|
| python-add | ✗ | ✗ | ✓ | ✓ | ✗ | ✓ |
| cli-help-flag | ✓ | ✓ | **✗** | ✓ | ✓ | ✗ |
| test-output-first | ✗ | ✗ | ✓ | ✓ | ✗ | ✗ |
| docs-symbol-rename | ✗ | ✗ | ✓ | ✓ | ✓ | ✓ |
| read-only-audit | ✓ | ✓ | ✓ | **✗** | ✗ | ✗ |
| no-tool-diagnosis | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |

The truth table is what justifies the GO. The `asymmetric` cell is a
+50pp recovery on 3 multi-tool fixtures (`python-add`,
`test-output-first`, `docs-symbol-rename`), netted against one regression
on `cli-help-flag` where the judge's veto-and-regen cycle consumed the
iteration budget. `symmetric-strong` shows a different regression
(`read-only-audit`) where the strong judge drives over-engineering on a
trivial fixture.

## Mechanism

Three distinct effects compose the +33pp lift:

1. **Recovery from cheap-model tool-emission failure (dominant).** The
   three baseline failures all show `tool_calls=0` and `output_tokens`
   pinned at the max-tokens cap. The Sonnet judge catches these
   structural failures and forces regeneration; the Haiku judge
   (symmetric) often passes them sycophantically.
2. **Cheap-judges-cheap is flat** (0pp lift) — matches Snorkel's
   self-critique paradox prediction.
3. **Judge can destructively short-circuit runs.** Each veto consumes
   2 iterations (popped turn + regenerated turn). At
   `max_iterations: 8`, judge-heavy runs can leave 2 iterations for
   actual progress, which is what regressed `cli-help-flag`. Adversarial
   rubric and `retain` make this worse.

## Cost mechanism (the surprise)

Asymmetric costs *less* than baseline (−56%) because:

1. Failed baseline turns aren't free — they burn the full output cap
   producing malformed text the model can't recover from.
2. Judge calls are cheap (~$0.005 each: ~1400 input + ~44 output tokens
   at Sonnet 4.6 pricing).
3. Vetoes prevent the cascade. One judge call buys you ~2 wasted
   generator turns.

This is specific to the bad-baseline case. If the generator already
works, the judge is pure overhead (`symmetric-strong` is +$0.02 vs
baseline because Sonnet doesn't need the help).

## Recommended shipping config

```harn
agent_loop(message, system, {
  step_judge: {
    model: "anthropic/claude-sonnet-4-6",  // strongest available judge
    provider: "openrouter",                // or "anthropic" directly
    on_veto: "replace",                    // default; pop-and-regen validated
    max_attempts: 3,
    rubric: "default",                     // default; adversarial probe hurt
  },
})
```

Do **not** recommend `symmetric` pairings (cheap-cheap is flat,
strong-strong adds cost without gain).

## Caveats

- n=6 fixtures × 1 replicate per cell. One fixture flipping moves a
  cell by 17pp. Headline lifts have wide CIs.
- Text tool format only (OpenRouter Anthropic native tools not
  catalogued; see follow-up #2319). Native-format numbers may differ.
- `cache_read_tokens` was 0 across all calls — cache attribution
  through OpenRouter is broken or genuinely not engaging (#2320).
- Regression rate isn't in pre-registered metrics; the truth table
  surfaces it but the runner should track it natively (#2318).

## Go / no-go

| Criterion | Threshold | Measured | Verdict |
|---|---|---|---|
| asymmetric pass-rate lift | ≥ 15pp at ≤ 3× baseline cost | +33pp at 0.45× | **GO** |
| symmetric-cheap pass-rate lift | ≥ 10pp at ≤ 2× baseline cost | 0pp at 0.91× | not met |

## Follow-up issues

- #2317 — Haiku 4.5 text-format tool-emission collapse (root cause of
  3/6 baseline failures, independent of step_judge)
- #2318 — Eval runner: track per-cell regression rate
- #2319 — Provider catalog: openrouter:anthropic/claude-* native-tools
- #2320 — Cache attribution via OpenRouter
- burin-labs/burin-code#1155 — burin-code TUI adoption

Remaining (no separate issue, handled in next experiment round):

- Replicates 2-3 + 6 more burin-examples-targeted fixtures (~$5)
- Judge-architecture sweep (GPT-4.1-mini, DeepSeek V3.2)
- `read-only-audit` fixture brittleness investigation
- Iteration budget × `max_attempts` interaction documentation

## v2 audit fixes (this PR)

Three Anthropic-provider bugs surfaced while validating against
`--tool-format native` (which sidesteps Effect 1 entirely). All three
were silently HTTP-400-ing every native call to Anthropic and are
independent of `step_judge`:

1. **`x-harn-output-schema` extension stripped.** Anthropic's strict
   validator rejected the Harn-internal tool field. Mirror the
   `openai_compat.rs` sanitizer.
2. **`temperature` stripped when thinking is active.** Anthropic
   rejects `temperature != 1` when thinking is enabled; Haiku 4.5+
   auto-enables adaptive thinking. Callers no longer have to set
   `thinking: {mode: "disabled"}` defensively.
3. **Eval suite `max_tokens` bumped 1024 → 2048.** Anthropic requires
   `max_tokens > thinking.budget_tokens`; the previous cap conflicted
   with Haiku 4.5's auto-applied budget.

Both provider fixes ship with regression tests. The headline experiment
numbers above don't change (original run used text format via
OpenRouter, which doesn't hit any of these constraints), but the fixes
unblock a future v2.1 experiment that re-runs the GO cell against
direct Anthropic native tools to measure Effect 1's contribution.

## Raw data

`summary.json` per cell in `results/main-grid-2026-05-23/`.
Per-run JSONL + transcript_events are gitignored under the
timestamped run dir.
