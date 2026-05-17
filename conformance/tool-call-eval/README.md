# Tool-Call Eval Dataset

This directory contains Harn's PEAR-style tool-call accuracy fixture set for:

- aggregate pass-rate scoring across many tool-call cases,
- planner, binder, and total latency measurement,
- multi-model comparison through `harn eval tool-calls`,
- exact and refusal scoring against expected tool-call decisions.

Run the offline smoke path with the mock provider:

```sh
cargo run --bin harn -- eval tool-calls \
  --dataset conformance/tool-call-eval \
  --planner mock:mock \
  --output .harn-runs/tool-call-eval/latest
```

Run a planner+binder cell with real providers:

```sh
cargo run --bin harn -- eval tool-calls \
  --dataset conformance/tool-call-eval \
  --planner provider=openrouter,model=google/gemma-4-26b-a4b-it \
  --binder provider=cerebras,model=gpt-oss-120b \
  --judge-model anthropic:claude-haiku-4-5 \
  --output .harn-runs/tool-call-eval/gemma-cerebras
```

The command writes:

- `summary.json`: aggregate pass rate, p50/p99 planner latency, optional binder latency, total
  latency, total cost, and one per-case verdict row.
- `per_case.jsonl`: one full drill-down record per case, including raw planner/binder responses and
  token/cost accounting.

Regression check:

```sh
cargo run --bin harn -- eval tool-calls regression-check \
  --current .harn-runs/tool-call-eval/latest/summary.json \
  --against conformance/tool-call-eval/baselines/mock-planner.summary.json \
  --max-drop-pp 2.0
```

Baseline files keep only stable pass-rate fields; run outputs keep volatile per-case latencies in
`.harn-runs/`.

## Case Format

Cases are JSON objects under `cases/`; files may contain one object or an array.

```json
{
  "id": "example",
  "prompt": "Search for Harn release notes.",
  "tools": [
    {
      "name": "search",
      "description": "Search documents.",
      "parameters": {
        "query": {"type": "string"}
      }
    }
  ],
  "expected": {
    "kind": "exact",
    "name": "search",
    "args": {"query": "Harn release notes"}
  },
  "source": "harn_hand_authored",
  "tags": ["simple"]
}
```

`parameters` uses the same per-argument schema fragments accepted by Harn tool definitions. Exact
cases require a single normalized tool call with deep-equal arguments; numeric values tolerate tiny
float representation differences. Refusal cases pass only when no tool call is emitted and the final
text matches `reason_must_match`.

Predicate cases are supported by the runner but intentionally absent from the initial checked-in
50-case set so the default dataset can run without judge spend.

## Attribution

The dataset mixes:

- Harn conformance-inspired cases, named with `source: harn:...`.
- Hand-authored binder edge cases, named with `source: harn_hand_authored:...`.
- BFCL-style synthetic cases, named with `source: bfcl_style_synthetic:...`.

The BFCL-style cases follow the public Berkeley Function Calling Leaderboard category structure
(simple, multiple, REST, SQL, chatting/irrelevance) but do not copy BFCL rows or expected answers.
BFCL is published under Apache-2.0 on Hugging Face and documented at:

- https://huggingface.co/datasets/gorilla-llm/Berkeley-Function-Calling-Leaderboard
- https://gorilla.cs.berkeley.edu/leaderboard.html

## Adding Cases

Add a stable `id`, keep prompts self-contained, declare only the tools visible to the planner, and
include tags that let smoke runs focus on a slice with `--filter`. Use exact cases for deterministic
tool decisions and refusal cases when the correct behavior is no tool call.
