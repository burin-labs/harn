# Step-Judge Experiment

Empirical go/no-go for the `agent_step_judge` per-turn LLM critique
primitive (`crates/harn-stdlib/src/stdlib/agent/step_judge.harn`).

## Question

Does a per-turn LLM judge that critiques each assistant response and
prompts regeneration (Reflexion-style, pop-and-regen by default) lift
coding-agent task success enough to justify the cost overhead?

## Hypothesis (pre-registered)

The 2024-25 literature is split:

- **Pro:** PRM-style verifiers and process reward models lift base
  models on math/code reasoning. Reflexion-style critique loops show
  positive results on HumanEval with executable test feedback.
- **Con:** Huang et al. (DeepMind) and Snorkel's "self-critique
  paradox" both show that same-model self-critique on confident
  responses can degrade performance. Cheap-judges-cheap is the
  weakest cell.

We expect the asymmetric cell (Haiku generator + Sonnet judge) to
show the largest lift; the symmetric-cheap cell to be neutral-to-
negative; the symmetric-strong cell to be near-zero (Sonnet
self-critique).

## Design

**Main grid (4 cells × 6 fixtures × 3 replicates = 72 runs, ~$10):**

| Cell | Generator | Judge |
|---|---|---|
| `baseline-cheap` | Haiku 4.5 | — |
| `symmetric-cheap` | Haiku 4.5 | Haiku 4.5 |
| `asymmetric` | Haiku 4.5 | Sonnet 4.6 |
| `symmetric-strong` | Sonnet 4.6 | Sonnet 4.6 |

**Probes (run only against the winning cell, 1 replicate each, ~$5):**

- `probe-rubric-adversarial` — adversarial vs neutral rubric
- `probe-transcript-shape` — `retain` (Reflexion) vs `replace`
  (pop-and-regen) on_veto

(judge-arch sweep deferred to follow-up if the asymmetric cell wins
by ≥5pp — we want to keep the experiment cheap.)

**Fixtures:** the existing `harn eval coding-agent` suite (6 tasks
spanning multi-tool, one-tool, no-tool sequences). Adding
burin-examples-targeted tasks across additional languages is filed as
a follow-up — these existing fixtures are deterministic and already
exercise the agent loop in the same way.

## Go / no-go decision

| Outcome | Decision |
|---|---|
| asymmetric pass-rate lift ≥ 15pp at cost ≤ 3× baseline | **GO** — ship as recommended opt-in |
| symmetric-cheap pass-rate lift ≥ 10pp at cost ≤ 2× baseline | **GO** — recommend both presets |
| 5pp ≤ lift < threshold | **SHIP AS OPT-IN** with mixed-evidence note |
| lift < 5pp OR degraded | **NO-GO** — primitive lands as `@experimental` |

The primitive itself ships regardless — it's a small, well-isolated
stdlib addition and the experiment is part of the same PR. The
decision is about whether to recommend it.

## Running the experiment

Requires OpenRouter API key in env (or `~/projects/burin-code/.env`).

```sh
# Mock smoke test first (no credits)
./experiments/step-judge/run.sh --mock

# Full main grid + probes (~$15 budget cap)
./experiments/step-judge/run.sh

# Just one cell (for debugging)
./experiments/step-judge/run.sh --cell asymmetric --replicates 1
```

Outputs land under `experiments/step-judge/results/<timestamp>/`,
with one subdir per cell × replicate and an aggregated `REPORT.md`.

## Files

- `run.sh` — bash driver that invokes `harn eval coding-agent` once
  per cell × replicate, with the right `--step-judge`, `--model`, and
  `--run-label` flags.
- `aggregate.harn` — reads each invocation's `summary.json`, groups
  by cell + replicate, computes pass-rate lift vs baseline, writes
  `REPORT.md`.
- `REPORT.md` — generated experiment report (committed when the
  experiment is run).
