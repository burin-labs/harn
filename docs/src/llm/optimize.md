# Prompt optimization

`std/llm/optimize` provides a deterministic prompt-search loop for tuning an
instruction against an eval set:

```harn
import { optimize_prompt } from "std/llm/optimize"

pipeline default() {
  let result = optimize_prompt({
    base_prompt: "Answer the question.",
    eval_set: [
      {id: "add", input: "2 + 2", expected: "4"},
      {id: "mul", input: "3 * 3", expected: "9"},
    ],
    metric: { ctx ->
      if contains(lowercase(ctx.prompt), "calculate") {
        return 1.0
      }
      return 0.0
    },
    trials: 4,
    instruction_proposals: [
      "Answer the question.",
      "Calculate carefully and answer only with the result.",
    ],
  })

  log(result.best_prompt)
  log(result.best_score)
}
```

The optimizer searches over `(instruction, demos)` candidates. Instruction
proposals come from `std/llm/refine` via `propose_instructions(...)`; callers can
pass `instruction_proposals`, `proposal_fn`, or LLM options for structured
proposal generation. Eval scoring is delegated to `parallel_judge(...)` from
`std/llm/judge`, so each candidate's eval cases can run concurrently.

`optimize_prompt(config)` returns:

| Field | Description |
|---|---|
| `best_prompt` | Rendered prompt for the best observed candidate |
| `best_score` | Mean eval-set score for `best_prompt` |
| `best_candidate` | `{instruction, demos, prompt, index}` for the winning candidate |
| `trace` | Trial-by-trial observations, case scores, and acquisition metadata |
| `ranked` | Observed candidates sorted by score |
| `candidates` | Full discrete candidate space considered for search |

The acquisition loop is intentionally inspectable. It evaluates a seed candidate
first, then selects unobserved candidates using an expected-improvement score
derived from a small similarity-weighted surrogate over prior observations.

`budget.max_concurrent` caps parallel eval cases. `budget.max_trials` and
`budget.max_evaluations` cap the total search. For conformance and local tests,
use explicit `instruction_proposals` and a deterministic metric closure to avoid
real LLM calls.
