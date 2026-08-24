# Run an A/B experiment

You changed a prompt, a cache policy, or a model route, and you want to know
whether it actually helped. Harn has three surfaces for that. Pick by what you
are comparing.

| You are comparing | Use | Gives you |
| --- | --- | --- |
| Two configurations, and you need a defensible answer | [`std/eval/experiment`](#the-experiment-contract) | randomized-block assignment, anytime-valid decisions, guardrails, a spend ceiling, and an explicit promotion gate |
| Two versions of one pipeline | [`harn eval --structural-experiment`](#compare-two-runs-of-one-pipeline) | a paired baseline-vs-variant summary over two runs |
| One prompt across several models | [`harn eval prompt --fleet`](#compare-one-prompt-across-models) | per-model rendering, output, and optional judge scoring |

The first is a statistical contract you call from Harn code. The other two are
CLI commands. They are unrelated implementations — reach for the one that
matches your question.

## The experiment contract

`std/eval/experiment` is for the case where you will act on the result. It
refuses to let you cheat: case sets are frozen at registration, the gate cases
cannot be spent during tuning, assignment is deterministic under a seed, the
family error budget is split across every candidate and guardrail, and
promotion to the holdout set is a separate explicit step.

### Declare the manifest

The manifest states the hypothesis, the arms, the metric you care about, the
guardrails that must not regress, how trials are assigned, which cases belong
to tuning versus the frozen gate, and the ceilings.

```harn,check
const MANIFEST = {
  schema: "harn.experiment.v1",
  experiment_id: "prompt-cache-policy",
  hypothesis: "Caching the system prompt improves success without raising cost.",
  owner: "eval-team",
  baseline: {id: "baseline", config: {cache: "off"}, complexity: 0},
  candidates: [{id: "cached", config: {cache: "on"}, complexity: 1}],
  decision: {delta: 0.05, epsilon: 0.1, ladder: [3, 5, 10, 80]},
  metrics: {
    primary: {id: "success", direction: "up", bounds: {lo: 0.0, hi: 1.0}},
    guardrails: [
      {
        id: "cost",
        direction: "down",
        bounds: {lo: 0.0, hi: 5.0},
        alarm: {kind: "absolute", threshold: 0.1},
      },
    ],
  },
  assignment: {
    mode: "randomized_block",
    seed: "seed-42",
    blocking_factors: ["host", "time_slot"],
  },
  splits: {
    iterate: {id: "tune", digest: "tune-v1", cases: ["case-a", "case-b"]},
    gate: {id: "holdout", digest: "holdout-v1", cases: ["case-z"]},
    promotion: "explicit",
  },
  budget: {max_spend_usd: 10.0, max_trials_per_case: 80},
}
```

`bounds` on every metric is required, not decoration: anytime-valid inference
cannot be honest over an undeclared support. `budget` is a hard ceiling on the
whole experiment, and promotion carries the spend already used into the gate
phase rather than resetting it.

The validation context is what your host supports, and it is checked against
the manifest — asking to block on a factor the host cannot observe is rejected
at validation rather than producing a quietly meaningless result:

```harn,ignore
const CONTEXT = {
  supported_blocking_factors: ["host", "time_slot"],
  host_identity_available: true,
}
```

### Register and assign

`register_experiment` freezes both case sets. `plan_assignments` produces one
balanced block containing the baseline and every candidate exactly once, and it
is deterministic — replaying the same case, trial, and block gives the identical
plan. `realize_assignment` records which arm a host actually ran and refuses a
block other than the one it was assigned.

```harn,ignore
const valid = unwrap(validate_experiment_manifest(MANIFEST, CONTEXT))
const registration = register_experiment(valid)

const block = {host: "host-a", time_slot: "slot-1"}
const plan = plan_assignments(registration, "case-a", 0, block)
const realized = realize_assignment(plan, "cached", block)
```

Calling `plan_assignments` with one of the gate cases during the iterate phase
throws. That is the point: you cannot spend the holdout set while tuning.

### Decide and promote

Feed paired observations to `decide_experiment`. Each observation carries the
baseline and treatment value for every metric plus both realized assignments,
so the decision keeps its own evidence.

```harn,ignore
const decision = decide_experiment(
  registration,
  {observations: observations, phase_spend_usd: 0.32, budget_spent: true},
)

const promoted = promote_experiment(registration, decision)
```

Running the whole flow over 160 paired observations:

```text
phase=iterate
iterate cases=[case-a, case-b]
verdict=ITERATE_WINNER
winner=cached
promotion_required=true
gate phase=gate
gate cases=[case-z]
```

`promotion_required` is always true under `promotion: "explicit"` — winning the
tuning phase does not ship anything. `promote_experiment` builds a second
registration scoped to the frozen gate cases and to the baseline plus the single
winner, so the holdout run compares two arms rather than re-running the field.

Other verdicts you will see: `BASELINE` when a candidate's upper confidence
bound falls below the practical-equivalence band, and a per-candidate
`regressed_on_primary` status that lets a scheduler stop a losing arm without
`std/eval` knowing anything about where work runs.

The full surface, including the manifest's every field and the guardrail alarm
semantics, is in [`std/eval/experiment`](../modules.md#stdevalexperiment). For
turning a stated hypothesis into one of these manifests without letting model
output become executable authority, see
[Compile a bounded experiment](./compile-hypothesis.md) and
[ADR-0007](../adr/0007-hypothesis-compiler-ownership.md).

## Compare two runs of one pipeline

When the two things you are comparing are two versions of the same pipeline,
`harn eval --structural-experiment` runs it twice in isolated run directories —
once as the baseline, once with `HARN_STRUCTURAL_EXPERIMENT=<spec>` set — and
prints a paired summary:

```bash
harn eval --llm-mock fixtures.jsonl --structural-experiment doubled_prompt pipeline.harn
```

```text
Structural experiment: doubled_prompt
Cases: 1
- tiny [Say hi.]
  baseline: PASS
  variant: PASS
  diff identical: false
  stage diffs: 0
  tool diffs: 0
  observability diffs: 6
Baseline 1 / 1 passed
Variant 1 / 1 passed
```

This reads workflow run records, so the pipeline must call `workflow_execute`;
a plain `harn run`-style pipeline produces nothing for it to compare and the
command reports that one side was empty. Pass `--llm-mock` to keep both runs
deterministic.

## Compare one prompt across models

`harn eval prompt` renders a `.harn.prompt` against a fleet of models, and
optionally runs and scores it:

```bash
harn eval prompt prompts/agent.harn.prompt \
  --fleet claude-sonnet-5,gpt-5,ollama:qwen3.5 \
  --mode judge
```

`--mode render` only renders against each model's capability profile,
`run` renders and executes, and `judge` adds LLM-as-judge equivalence scoring.
`--fleet-name` uses a named fleet from `[eval.fleets.<name>]` in `harn.toml`,
and `--output html -o report.html` writes a shareable report.

This is a comparison tool, not an experiment: there is no assignment policy, no
confidence sequence, and no spend ceiling. Use it to see how a prompt lands
across models, then use the experiment contract above if you need to defend the
resulting change. See
[`harn eval prompt`](../cli-reference.md#harn-eval-prompt) for the full flag
set.
