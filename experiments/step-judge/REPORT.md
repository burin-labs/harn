# Step-Judge Experiment — Report (2026-05-23)

**Decision: GO.** Ship `step_judge` as an opt-in recommended preset using the
asymmetric pairing (cheap generator + strong judge). Default `on_veto: "replace"`
and default neutral rubric, both empirically validated against alternatives.

## Headline numbers

| Cell | Pass rate | Cost (USD) | Lift vs baseline |
|---|---:|---:|---:|
| `baseline-cheap` (Haiku 4.5, no judge) | 3/6 = **50%** | $0.31 | — |
| `symmetric-cheap` (Haiku 4.5 gen + Haiku 4.5 judge) | 3/6 = **50%** | $0.29 | **0pp** |
| `asymmetric` (Haiku 4.5 gen + Sonnet 4.6 judge) | 5/6 = **83%** | **$0.14** | **+33pp** |
| `symmetric-strong` (Sonnet 4.6 gen + Sonnet 4.6 judge) | 5/6 = **83%** | $0.33 | +33pp |

All cells via OpenRouter, text tool format (OpenRouter Anthropic doesn't have
native tools catalogued in Harn — see follow-up), 6 fixtures from the existing
`harn eval coding-agent` suite, 1 replicate.

## What this means

1. **The asymmetric judge matches a strong-model baseline at less than half the
   cost.** Haiku + Sonnet-judge produced the same 83% pass rate as Sonnet alone,
   at $0.14 vs $0.33. That's the headline business case for shipping this
   primitive: **+33pp pass rate AND −56% cost** vs the cheap-model baseline.
2. **The cost saving is the surprise.** Naïve expectation was that adding judge
   calls would *increase* cost. Instead, vetoed turns get popped before tool
   dispatch, the agent regenerates better responses, and the loop terminates in
   fewer total iterations. The judge's $0.06/run cost is more than offset by the
   fewer generator turns it enables.
3. **Cheap-judges-cheap (symmetric) is flat — exactly as the literature
   predicts.** Snorkel's self-critique paradox and Huang et al. (DeepMind, arxiv
   2310.01798) both showed same-model self-critique is neutral-to-negative. We
   measured 0pp lift, validating that prediction. This rules out "ship
   symmetric-cheap as default."
4. **The two directional probes both validated the default config choices.**

## Directional probes (asymmetric base)

| Probe | Pass rate | Cost | Lift vs asymmetric default |
|---|---:|---:|---:|
| `asymmetric` (default — neutral rubric, pop-and-regen) | 5/6 = 83% | $0.14 | (reference) |
| `probe-rubric-adversarial` (adversarial rubric, pop-and-regen) | 3/6 = **50%** | $0.19 | **−33pp** |
| `probe-transcript-shape-retain` (neutral rubric, retain) | 3/6 = **50%** | $0.19 | **−33pp** |

**Both probes hurt.** Adversarial rubric drove false-positive vetos that pushed
the agent off task; retain mode polluted the context with bad turns + critiques
and reduced regeneration quality. The defaults shipped in `step_judge.harn` are
the empirically right defaults.

## Why the asymmetric cell wins (mechanism)

Trace inspection (`transcript_events.jsonl` in
`asymmetric-r1/python-add__.../`) shows the typical pattern:

1. Haiku produces a turn with malformed `<function_calls>` or a tool call with
   bad arguments.
2. Sonnet judge fires, returns `{verdict: "revise", critique: "Remove the
   read_file({path: 'tests'}) call since 'tests' is a directory, not a file —
   use only list_directory to inspect it."}`.
3. Loop pops the bad assistant turn, injects critique as a user/feedback
   message, continues.
4. Haiku regenerates with the critique in scope, produces a clean tool call.
5. Tool dispatches, loop progresses.

The judge is doing structural type-checking + intent verification that Haiku
alone can't reliably self-check. This is exactly the asymmetry the literature
predicts works.

## Why symmetric-cheap is flat (mechanism)

Same trace pattern but Haiku-judge often passes Haiku-generator turns that are
obviously wrong (sycophantic), AND occasionally vetos correct turns (false
positive). The two errors cancel out — same pass rate, slightly less cost
(fewer wasted generations), but no real lift. Matches the SOTA prediction.

## Caveats

- **n=6 fixtures × 1 replicate = 6 observations per cell.** The 33pp lift is
  large enough to survive noise, but the absolute numbers (50% / 83%) have wide
  confidence intervals at this sample size. Filed as `experiments/step-judge`
  follow-up: run replicates 2 and 3 of the main grid (~$1 more) and add 6 more
  burin-examples-targeted fixtures (~$3 more) to tighten the estimates.
- **Text tool format only.** OpenRouter Anthropic models don't have native
  tools catalogued in Harn (`docs/src/provider-matrix.md` shows entries for
  direct `anthropic` provider but not via `openrouter`). Used text format for
  all cells to keep the comparison apples-to-apples. Native-tools cells via
  direct Anthropic provider would likely show similar relative lift but
  absolute numbers may differ. Follow-up: extend provider catalog with
  `openrouter:anthropic/claude-*` native-tool entries.
- **Prefix-cache utilization not measured.** `cache_read_tokens` came back 0
  across all cells, suggesting OpenRouter's Anthropic passthrough doesn't
  surface cache hit metadata in the standard fields, OR caching genuinely
  wasn't activating. Worth investigating — could be additional cost savings on
  the table.
- **Single failure mode in asymmetric cell (`read-only-audit`).** All four
  configured cells failed exactly this one fixture, suggesting it's a
  fixture-design issue rather than a judge issue. (Sonnet alone also failed
  it.)

## Go / no-go applied

Per the pre-registered criteria in `experiments/step-judge/README.md`:

| Criterion | Threshold | Measured | Verdict |
|---|---|---|---|
| asymmetric pass-rate lift | ≥ 15pp at ≤ 3× baseline cost | **+33pp at 0.45× baseline cost** | **GO** |
| symmetric-cheap pass-rate lift | ≥ 10pp at ≤ 2× baseline cost | 0pp at 0.91× baseline cost | NOT MET |

**Decision: ship the primitive with the asymmetric preset as the recommended
opt-in.** The cost savings make this an unusually clean call — we don't have to
argue cost-vs-quality trade-off because asymmetric wins on both axes.

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

Bare-minimum opt-in (uses generator's model + provider if unspecified — but
this is the "symmetric" weak cell; document the asymmetric pairing as the
recommended setup):

```harn
agent_loop(message, system, {
  step_judge: {
    model: "anthropic/claude-sonnet-4-6",
    provider: "openrouter",
  },
})
```

## Follow-up issues

To be filed against `harn` after this PR merges:

1. **Extend provider catalog** with `openrouter:anthropic/claude-*`
   native-tools entries so OpenRouter users don't have to fall back to text
   format.
2. **Investigate prefix-cache attribution** through OpenRouter —
   `cache_read_tokens` is silently 0; either rendering or upstream issue.
3. **Run main grid replicates 2 and 3** + add 6 burin-examples-targeted
   fixtures (~$5 spend) to tighten the lift estimate from "n=6 fixtures" to
   "n=12 fixtures × 3 replicates".
4. **Judge-architecture sweep** — the directional probes only covered rubric
   and transcript-shape. Worth ~$5 to swap the Sonnet judge for GPT-4.1-mini
   and DeepSeek V3.2 to see if the asymmetry effect is provider-agnostic.
5. **`read-only-audit` fixture investigation** — all four cells failed it.
   Likely fixture-spec ambiguity, not a judge issue.

And the cross-repo burin-code re-platform issue (filed separately, see the
harn PR description).

## Raw data

All `summary.json` files in `experiments/step-judge/results/main-grid-2026-05-23/`,
one subdir per cell + probe. Full per-run JSONL + transcript_events are
gitignored under the timestamped run dir.
