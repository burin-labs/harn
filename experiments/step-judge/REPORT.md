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

## Per-fixture truth table

Headline pass rate hides where the cells actually diverge. Truth table
(✓ = passed, ✗ = failed) reconstructed from each cell's `summary.json`:

| Fixture | baseline-cheap | symmetric-cheap | asymmetric | symmetric-strong | probe-adversarial | probe-retain |
|---|:---:|:---:|:---:|:---:|:---:|:---:|
| `python-add` | ✗ | ✗ | ✓ | ✓ | ✗ | ✓ |
| `cli-help-flag` | ✓ | ✓ | **✗** | ✓ | ✓ | ✗ |
| `test-output-first` | ✗ | ✗ | ✓ | ✓ | ✗ | ✗ |
| `docs-symbol-rename` | ✗ | ✗ | ✓ | ✓ | ✓ | ✓ |
| `read-only-audit` | ✓ | ✓ | ✓ | **✗** | ✗ | ✗ |
| `no-tool-diagnosis` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |

Key observations the truth table forces (which the headline numbers hide):

- **`asymmetric` regressed `cli-help-flag`** — a fixture that the baseline
  Haiku alone solved. The judge intervention cut the run short
  (5 iterations vs baseline's 6, 8 tool calls vs baseline's 23). So the
  "+33pp" win is actually a +50pp recovery on 3 multi-tool fixtures (python-add,
  test-output-first, docs-symbol-rename) netted against a regression on 1.
  **Regression rate matters and was not in the pre-registered metrics.**
- **`symmetric-strong` regressed `read-only-audit`** — the trivial
  "read one file, say no edits" task. Sonnet+Sonnet emitted 8 tool calls and
  only 30 output tokens before timing out. Strong-model overthinking + strong
  judge demanding more rigor is its own pathology, separate from the
  cheap-judge-cheap pathology.
- **Both probes regressed `read-only-audit` too**, plus `python-add` and
  `test-output-first`. The adversarial rubric drove false-positive vetoes;
  the retain shape polluted context with bad turns. Both effects compound on
  trivial tasks where the right move is "do nothing more."
- **`no-tool-diagnosis` passes in every cell**, including baseline. Pure prose
  responses never trigger the judge's failure modes — confirms the judge
  primitive is benign-safe for non-tool workflows.

## Directional probes (asymmetric base)

| Probe | Pass rate | Cost | Lift vs asymmetric default |
|---|---:|---:|---:|
| `asymmetric` (default — neutral rubric, pop-and-regen) | 5/6 = 83% | $0.14 | (reference) |
| `probe-rubric-adversarial` (adversarial rubric, pop-and-regen) | 3/6 = **50%** | $0.19 | **−33pp** |
| `probe-transcript-shape-retain` (neutral rubric, retain) | 3/6 = **50%** | $0.19 | **−33pp** |

Both probes hurt. Adversarial rubric drove false-positive vetoes that pushed
the agent off task; retain mode polluted the context with bad turns + critiques
and reduced regeneration quality. The defaults shipped in `step_judge.harn` are
the empirically right defaults.

## Mechanism analysis — what the transcripts actually show

The headline "asymmetric judge lifts pass rate" wraps three distinct sub-effects
that the per-run telemetry exposes:

### Effect 1 — recovery from Haiku text-format tool-call collapse

This is the **dominant** effect and was not anticipated in the pre-registered
design. Across baseline-cheap and symmetric-cheap, the 3 failing fixtures
(`python-add`, `test-output-first`, `docs-symbol-rename`) all show an identical
telemetry signature:

| Cell | Fixture | iters | tool_calls | rejected | input_tokens | output_tokens |
|---|---|---:|---:|---:|---:|---:|
| baseline-cheap | python-add | 3 | **0** | 0 | 8496 | **3072** |
| baseline-cheap | test-output-first | 3 | **0** | 0 | 6404 | **3072** |
| baseline-cheap | docs-symbol-rename | 2 | 16 | **7** | 4796 | 1741 |
| symmetric-cheap | python-add | 3 | **0** | 0 | 8496 | **3072** |
| symmetric-cheap | test-output-first | 3 | **0** | 0 | 6404 | **3072** |
| symmetric-cheap | docs-symbol-rename | 2 | 16 | **7** | 4796 | 1741 |

The `tool_calls = 0` + `output_tokens = 3072` (hitting the max-tokens cap)
fingerprint is unmistakable: **Haiku 4.5 in text-format tool mode emits a flood
of malformed/empty `<function_calls>` tags instead of structured calls,
exhausts the output budget, and silently terminates the loop without
dispatching any tool.** Symmetric-cheap inherits the same failure — Haiku-judge
either passes the malformed response (sycophancy) or vetoes it but the
regeneration hits the same pathology, since the same model has the same bug.

Direct evidence from an earlier sanity run's transcript
(`/private/tmp/step-judge-sanity`) where Sonnet judged Haiku:

> Judge feedback (verdict: revise):
> *"The response is entirely broken: it emits only a flood of empty
> `<function_calls>` tags without any actual tool call content (no function
> name, no arguments). Regenerate with a single, properly formed tool call …"*

The Sonnet judge in asymmetric correctly diagnoses this structural failure
and the pop-and-regen pushes Haiku into a working state. The
`docs-symbol-rename` case is similar but a different sub-mode: 16 tool calls
but 7 rejected (43%) — the model attempts tool calls but malforms them
badly, terminating after 2 iterations.

This finding is independent of step_judge and worth a standalone follow-up
(filed below).

### Effect 2 — strong judge causes strong-model regressions

`symmetric-strong` solves all four multi-tool fixtures (per-fixture telemetry
confirms Sonnet handles tool-format cleanly: 50–80 tool calls, ~12–15% rejected
rate, no max-token cap hits). But it regressed `read-only-audit`: 8 tool calls,
4 iterations, only 30 output tokens before failing. The likely chain — Sonnet
generator does the trivial audit correctly in 1–2 calls; Sonnet judge
demands more rigorous evidence; pop-and-regen drives the model into
over-exploration until iteration cap. Cheap models pass this fixture because
they don't over-engineer in the first place.

### Effect 3 — judge can destructively short-circuit runs

`cli-help-flag` is the cleanest case. Baseline-cheap solved it in 6
iterations / 23 tool calls / 6 rejected. Asymmetric attempted it in 5
iterations / 8 tool calls / 3 rejected and **failed**. The judge's
veto-and-regen cycle cut the run short — each veto consumes 2 iterations
(the popped turn + the regenerated turn), and with `max_iterations=8` the
agent had no headroom. This is the **failure mode the GO recommendation
needs to be honest about**: the +33pp net lift includes a regression
that breaks one previously-passing fixture.

Adversarial rubric (probe) makes this worse — `python-add` and
`test-output-first` both regressed because the adversarial rubric drives more
vetoes, eating more iteration budget.

## Cost mechanism (the surprise)

Naïve expectation: judge calls are pure cost overhead. Observed: asymmetric
costs *less* than baseline. Why this works:

1. **Failed turns aren't free.** Baseline-cheap's 3 failed runs each burned
   3072 output tokens of malformed text trying-and-failing to emit tool
   calls. That's the bulk of the $0.31 baseline cost.
2. **Judge calls are cheap.** From the sanity-run typed_checkpoint telemetry:
   judge input averaged ~1400 tokens, output averaged ~44 tokens
   (verdict + reasoning + critique JSON), wall-clock ~1.5–3s per call.
   At Sonnet 4.6 prices that's ~$0.005/call. With ~5–10 judge calls per
   fixture, judge overhead is ~$0.03–0.05/run.
3. **Vetoes prevent wasted generations.** Each pop saves a future generator
   call that would have been spent recovering from the bad turn or, worse,
   was already trapped in the max-output-tokens death spiral.
4. **Asymmetric ≠ a free win.** The cost saving is specific to the
   bad-baseline case. If your generator already works, the judge is pure
   overhead (`symmetric-strong` is +$0.02 vs baseline-cheap and would be
   worse if Sonnet didn't recover the 3 failing fixtures).

## Caveats

- **n=6 fixtures × 1 replicate = 6 observations per cell.** A single fixture
  flipping pass/fail moves a cell by 17pp. Headline lifts of +33pp could
  shrink to +17pp or balloon to +50pp at this sample size. Replicates 2–3 +
  6 more fixtures (~$5 of API spend) would tighten estimates.
- **Text tool format only.** OpenRouter Anthropic models don't have native
  tools catalogued in Harn. The text-format collapse pathology (Effect 1)
  might disappear or shrink under native tools. Filed as follow-up.
- **Prefix-cache utilization not measured.** `cache_read_tokens` came back 0
  across all judge calls (verified in raw transcripts), suggesting OpenRouter's
  Anthropic passthrough doesn't surface cache hit metadata. Could be additional
  cost savings on the table — the judge would benefit from prefix cache because
  every call to the same session shares a long stable prefix.
- **Regression rate not in pre-registered metrics.** asymmetric broke
  `cli-help-flag`; symmetric-strong broke `read-only-audit`. Future
  experiments should track baseline-pass-cell-fail explicitly, not just
  net lift. Filed as follow-up.
- **Iteration-budget × judge interaction is structural.** Each veto consumes
  2 iterations. Default `max_attempts: 3` × 2 iterations = up to 6 vetoes
  per fixture could fit inside `max_iterations: 8` — leaving as little as
  2 iterations for actual progress. Documented as a config tradeoff in
  `step_judge.harn`.

## Go / no-go applied

Per the pre-registered criteria in `experiments/step-judge/README.md`:

| Criterion | Threshold | Measured | Verdict |
|---|---|---|---|
| asymmetric pass-rate lift | ≥ 15pp at ≤ 3× baseline cost | **+33pp at 0.45× baseline cost** | **GO** |
| symmetric-cheap pass-rate lift | ≥ 10pp at ≤ 2× baseline cost | 0pp at 0.91× baseline cost | NOT MET |

**Decision: ship the primitive as an opt-in with the asymmetric preset as the
documented recommendation.** Honest framing of the win: asymmetric recovers
3 multi-tool fixtures that fail because of an underlying Haiku text-format
tool-emission bug. Net of one regression (`cli-help-flag`), pass rate goes
3→5 and cost drops from $0.31 to $0.14. The primitive is a force-multiplier
for cheap models on tool-heavy tasks; it is *not* a free quality booster
for already-working setups.

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

Do **not** recommend `symmetric` pairings (cheap-cheap is flat by data,
strong-strong adds cost without gain when the generator already works).

## Follow-up issues

To be filed against `harn` after this PR merges:

1. **Haiku 4.5 text-format tool-emission collapse** (NEW, root-cause). Three
   of six fixtures fail baseline because Haiku in text-tool mode emits
   malformed `<function_calls>` tags + hits the 3072 output cap. Reproducible
   and independent of `step_judge`. Should be fixed in the text-tool prompt
   layer or by raising `max_output_tokens` for text-format runs.
2. **Eval runner: track per-cell regression rate** (NEW). Add a
   `regressions_vs_baseline` field to `summary.json` (count of fixtures
   that passed in `--baseline-comparison-against <run>` but failed in this
   run). Current "lift" metric hides destructive judge interactions.
3. **Extend provider catalog** with `openrouter:anthropic/claude-*`
   native-tools entries so OpenRouter users don't have to fall back to text
   format and trigger Effect 1.
4. **Investigate prefix-cache attribution** through OpenRouter —
   `cache_read_tokens` is silently 0 in both generator and judge telemetry.
5. **Run main grid replicates 2 and 3** + add 6 burin-examples-targeted
   fixtures (~$5 spend) to tighten the lift estimate.
6. **Judge-architecture sweep** — swap the Sonnet judge for GPT-4.1-mini and
   DeepSeek V3.2 to see if asymmetry is provider-agnostic.
7. **`read-only-audit` fixture brittleness** — the only fixture where
   strong-judge causes regression. Investigate whether the verifier is too
   strict on output format, or whether the fixture description invites
   over-engineering.
8. **Document iteration-budget × max_attempts interaction** — at the current
   defaults, judge vetoes can consume up to 6/8 iterations leaving no
   headroom for actual progress. Either raise default `max_iterations` when
   `step_judge` is enabled, or surface a warning in the runner.

And the cross-repo burin-code re-platform issue (filed separately, see the
harn PR description).

## Raw data

All `summary.json` files in `experiments/step-judge/results/main-grid-2026-05-23/`,
one subdir per cell + probe. Full per-run JSONL + transcript_events are
gitignored under the timestamped run dir.
