# Step-Judge Experiment — Report

**Last updated:** v3 + validator ablation (2026-05-24).

## TL;DR

The original v1 report's GO recommendation was wrong. Re-running the
experiment after fixing the OpenRouter Anthropic native-tools catalog
gap (#2319) and the macOS sandbox realpath issue (so the verifier
actually runs locally) shows that the +33pp lift previously attributed
to `step_judge` was almost entirely the judge masking a different bug:
Haiku 4.5 in text-tool-format mode does not reliably follow the tool
protocol. With native tools working correctly, **the step_judge
primitive provides 0 net lift on this 6-fixture suite at every
preset tested**.

**Updated decision: ship `step_judge` as a generic primitive but do
NOT recommend any preset.** It is a force-multiplier for cheap
generators on broken-tool-format runs (which the v1 data shows
clearly) and a wash otherwise. Document it as "use when you know
your generator is structurally weak; otherwise it's overhead."

Follow-up ablation with the 4-rule structural validator wired on by
default in the coding-agent suite, and `step_judge` forced off,
does not change that recommendation. Averaged across the three
Haiku text-mode samples, the validator recovers only about **one
third** of the remaining +16.7pp text-format lift that v1 had
credited to `step_judge`, and it does so via a different fixture
mix (recovering `read-only-audit` / `no-tool-diagnosis` while
regressing `cli-help-flag`).

The v1 numbers are preserved below for historical context and as
evidence of what step_judge can paper over when the generator path
is degenerate.

## v3 results (native tools, 6 fixtures × 1 replicate, 2026-05-24)

| Cell | Pass | Cost | Lift vs v3-baseline-native |
|---|---:|---:|---:|
| `baseline-native` (Haiku 4.5 alone) | 4/6 = **67%** | $0.14 | — |
| `symmetric-cheap` (Haiku + Haiku judge) | 3/6 = **50%** | $0.13 | **−16.7pp** |
| `asymmetric` (Haiku + Sonnet judge) | 4/6 = **67%** | **$0.11** | 0pp |
| `symmetric-strong` (Sonnet + Sonnet) | 4/6 = **67%** | $0.33 | 0pp |

| Fixture | baseline-native | sym-cheap | asym | sym-strong |
|---|:---:|:---:|:---:|:---:|
| python-add | ✓ | ✓ | ✓ | ✓ |
| cli-help-flag | ✓ | ✓ | ✓ | ✓ |
| test-output-first | ✗ | ✗ | ✓ | ✓ |
| docs-symbol-rename | ✓ | ✓ | ✓ | ✓ |
| read-only-audit | ✗ | ✗ | ✗ | ✗ |
| no-tool-diagnosis | ✓ | ✗ | ✗ | ✗ |

Two structural findings the truth table forces:

1. **The judge breaks `no-tool-diagnosis` at every preset.** That
   fixture runs at `max_iterations: 1` (it's a single-turn prose
   answer). The step judge vetoes the first turn → loop has 0
   iterations left for regeneration → `budget_exhausted`. This is a
   judge-vs-iteration-budget interaction the v1 experiment didn't
   surface because `no-tool-diagnosis` never had judging applied at
   the right code path in the original run. Filed as a follow-up:
   skip step_judge when remaining iterations ≤ 1.
2. **`read-only-audit` fails in baseline-native too.** This was the
   one fixture v1 marked as universally-failing-symmetric-strong;
   here it fails in all four native cells. The fixture verifier
   semantics may be too strict for how native-tools agents
   terminate. Filed.

## v3 vs v1: where did the +33pp go?

The v1 baseline ran with `--tool-format text` because OpenRouter
Anthropic native tools weren't catalogued (#2319). That hit the
Haiku text-format tool-emission collapse: 3 of 6 fixtures failed
because Haiku emitted prose narration instead of `<tool_call>` tags
(see #2317 thread for the full trace).

| Fixture | v1 baseline (text) | v3 baseline (native) | Diff |
|---|:---:|:---:|:---:|
| python-add | ✗ | ✓ | recovered by native |
| cli-help-flag | ✓ | ✓ | unchanged |
| test-output-first | ✗ | ✗ | unchanged |
| docs-symbol-rename | ✗ | ✓ | recovered by native |
| read-only-audit | ✓ | ✗ | regressed by native |
| no-tool-diagnosis | ✓ | ✓ | unchanged |

Net: native tools alone delivered +17pp (3→4 fixtures), and
recovered the same two fixtures that v1 attributed to step_judge
(`python-add`, `docs-symbol-rename`). Step_judge contributed the
remaining +16pp in v1 by recovering one more text-collapse failure
(`test-output-first`) — but with native tools that recovery is no
longer needed because the agent doesn't break in the first place.

| Decomposition of v1's +33pp lift | Contribution |
|---|---:|
| Native tools (eliminate text-format collapse) | ~+17pp |
| Step judge papering over remaining text-format failures | ~+16pp |
| **Step judge's value when tools work correctly** | **0pp** |

## Validator-vs-judge ablation

We reran the text-format grid with `--step-judge off
--structural-validator on`, using the 4-rule suite default
validator and `--baseline-comparison-against` to diff each cell
against its paired v3 native summary.

Because forcing `step_judge` off collapses `baseline-cheap`,
`symmetric-cheap`, and `asymmetric` to the same Haiku 4.5
generator config, those three cells should be read as replicate
samples of one condition rather than distinct strategies.

| Cell | Pass | Cost | Delta vs paired v3 native | Key regressions | Key recoveries |
|---|---:|---:|---:|---|---|
| `baseline-cheap` | 4/6 = **67%** | $0.07 | 0pp | `cli-help-flag` | `read-only-audit` |
| `symmetric-cheap` | 2/6 = **33%** | $0.05 | **−16.7pp** | `cli-help-flag`, `docs-symbol-rename`, `python-add` | `no-tool-diagnosis`, `read-only-audit` |
| `asymmetric` | 4/6 = **67%** | $0.05 | 0pp | `cli-help-flag`, `test-output-first` | `no-tool-diagnosis`, `read-only-audit` |
| `symmetric-strong` | 2/6 = **33%** | $0.11 | **−33.3pp** | `cli-help-flag`, `docs-symbol-rename`, `python-add`, `test-output-first` | `no-tool-diagnosis`, `read-only-audit` |

Truth table across the three equivalent Haiku samples:

| Fixture | v1 baseline (text) | validator-on Haiku text (3 samples) | v3 baseline (native) |
|---|:---:|:---:|:---:|
| python-add | ✗ | 2/3 | ✓ |
| cli-help-flag | ✓ | 0/3 | ✓ |
| test-output-first | ✗ | 0/3 | ✗ |
| docs-symbol-rename | ✗ | 2/3 | ✓ |
| read-only-audit | ✓ | 3/3 | ✗ |
| no-tool-diagnosis | ✓ | 3/3 | ✓ |

Three conclusions fall out of that table:

1. **The validator does real structural cleanup work.** It
   consistently recovers the two prose-only fixtures
   (`read-only-audit`, `no-tool-diagnosis`) that are most exposed to
   text-format protocol drift.
2. **It does not recover the v1 judge win.** `test-output-first`
   stayed 0/3, and `cli-help-flag` regressed in all three Haiku
   samples. So the validator is not a drop-in replacement for the
   old judge-on-text behavior.
3. **The pass-rate lift is too small and too noisy.** Averaging the
   three Haiku samples gives **10/18 = 55.6%** at an average cost of
   **$0.0569** per 6-fixture run. Against the v1 baseline text pass
   rate (**3/6 = 50%**), that is only **+5.6pp**. The report's
   "remaining step_judge effect" after native tools was **+16.7pp**,
   so the validator captures about **33%** of that lift, well below
   the **80%** threshold for changing the recommended shipping
   config.

## Updated GO / no-go

The pre-registered v1 criteria:

| Criterion | Threshold | v3 measured | Verdict |
|---|---|---|---|
| asymmetric pass-rate lift | ≥ 15pp at ≤ 3× baseline cost | 0pp at 0.79× baseline cost | **NOT MET** |
| symmetric-cheap pass-rate lift | ≥ 10pp at ≤ 2× baseline cost | −16.7pp at 0.93× baseline cost | **NOT MET** (regressed) |

Neither v3 cell meets the original lift threshold. The primitive ships
(it's wired, tested, and useful as opt-in tooling for users who
specifically want it) but the "recommended preset" advice from v1 is
withdrawn. The README is updated accordingly.

## What v1 got right, what it got wrong

Right:

- The directional probes (adversarial rubric, retain transcript shape)
  did empirically lose, validating the shipped defaults.
- The mechanism description in the v1 "Effect 1" section was accurate
  about the failure mode the judge was recovering from.
- The follow-up issues filed (#2317-#2320) all landed in v3.

Wrong:

- The headline +33pp was confounded by the catalog gap (#2319). The
  experiment should have caught this by running native-vs-text as
  separate axes — instead all cells were locked to text.
- "GO at asymmetric, ship as recommended opt-in" was bad advice. With
  the experiment confound removed, asymmetric is no better than the
  baseline on this fixture suite.
- The per-fixture truth table in v2 surfaced the regressions but the
  v2 conclusion still endorsed the original GO. v3 corrects this.

## v3 audit fixes (this run depends on)

- **#2317** (Haiku text-format collapse) — investigated, root cause
  documented (Haiku ignores the protocol entirely under text mode).
  Workaround: use native tools, unblocked by #2319. Closed.
- **#2318** (regression-rate metric) — `harn eval coding-agent` now
  has `--baseline-comparison-against <path>`. Used to generate the
  v3-vs-v1 truth table above. Closed.
- **#2319** (OpenRouter Anthropic native-tools catalog) — explicit
  `[[provider.openrouter]]` rules for `anthropic/claude-*`. The
  single most impactful fix for this experiment's accuracy. Closed.
- **#2320** (cache attribution) — extended extractors. The full
  resolution still requires cache_control forwarding through the
  OpenAI-compat adapter; tracked as a follow-up. Closed.

Also bundled with v3:

- **macOS sandbox** allowed `file-read-metadata` at top level + reads
  on `/var/select`/`/Library/Developer`. Without this the verifier
  could not run `python3` locally on macOS, which is why v1/v2 had
  to be run with text format (and why the catalog gap stayed
  invisible).

## Recommended shipping config (revised)

Step_judge ships as a generic opt-in primitive. There is no
recommended preset. If a user has documented evidence that their
generator structurally fails (e.g., consistent tool-format violations
that an external validator would catch but the loop doesn't), they
can wire `step_judge` with their own preset. The defaults
(`on_veto: "replace"`, neutral rubric) remain the right defaults for
that case.

```harn
agent_loop(message, system, {
  step_judge: {
    model: "anthropic/claude-sonnet-4-6",
    provider: "openrouter",
    on_veto: "replace",       // default; pop-and-regen validated
    max_attempts: 3,
    rubric: "default",        // default; adversarial probe lost in v1
    skip_when_iterations_remaining: 1,  // proposed v3.1, avoids the
                                        // no-tool-diagnosis regression
  },
})
```

`skip_when_iterations_remaining` is proposed but not implemented
yet — filed as a follow-up.

## Caveats

- n=6 fixtures × 1 replicate. A larger / different fixture suite
  could show step_judge providing real lift (e.g., on long
  multi-turn tasks where structural failures compound). The v1
  experiment used 6 because that's what shipped with the eval
  runner; expanding the suite is also a follow-up.
- All cells via OpenRouter. Direct `anthropic:*` provider would
  exercise different request paths (cache_control passthrough,
  prefill behavior) and may show different judge dynamics.
- Spend: v3 grid cost **$0.71** total ($0.14 + $0.11 + $0.13 +
  $0.33), well under the $15 cap. Adding replicates 2-3 + 6 more
  fixtures would cost ~$5 — proposed for v3.1.
- The validator ablation cost **$0.28** total
  ($0.07 + $0.05 + $0.05 + $0.11). Cheap, but still not strong
  enough evidence to revise the shipping recommendation.

## Raw data

- v1 (text, OpenRouter, 2026-05-23): `results/main-grid-2026-05-23/`
- v3 (native, OpenRouter, 2026-05-24): `results/main-grid-2026-05-24-v3/`
- v3 validator ablation (text, judge off, 2026-05-24):
  `results/main-grid-2026-05-24-v3-validator/`

Per-run JSONL + transcript_events are gitignored under the
timestamped run dirs. Only summary.json per cell is tracked.
