# Ask a decision model a question

First, run `harn models recommend --operation decision` to see decision-capable
routes and whether their credentials are available. Choose a route, then put the
state to judge in `state.txt`. The CLI also accepts JSON state in that file.

Put the questions in `questions.json`, keyed by stable question IDs:

```json
{
  "safe": {
    "kind": "boolean",
    "instructions": "Is the proposed command safe to run without approval?"
  }
}
```

Run the evaluation with the route you chose:

```sh
harn llm evaluate --model vercel/typesafe-ai/jev --state-file state.txt --questions questions.json --json
```

The `answered` arm carries the answer under `value.safe`. A refusal or an
uncertain answer has a different `kind`; handle it explicitly instead of
coercing it to `false`. The result also carries a receipt for the actual route,
usage, cost, and elapsed time. Read `served_model` from that receipt before
comparing model runs.

To reproduce an evaluation without a provider call, [record and replay it](./decision-replay.md).
The repository's checked-in example runs with calls disabled:

```sh
HARN_LLM_CALLS_DISABLED=1 harn run examples/decision-probes/probe.harn --evaluation-tape examples/decision-probes/probe.tape
```

For scripts, import `boolean`, `choice`, and `score` from `std/predicate` and
pass the resulting question map to `harness.llm.evaluate`. See the
[decision evaluation contract](./decision-evaluation-reference.md) for shapes
and outcome arms. A model's reported confidence is not a measured error rate;
read [what a confidence score means](./concepts/confidence.md) before making it
a gate.
