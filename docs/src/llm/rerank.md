# LLM reranking

`std/llm/rerank` provides pairwise reranking helpers for cases where a
single scalar score is brittle, plus a low-level confidence helper for
models that expose token log probabilities.

```harn
import { pairwise_rerank, self_certainty } from "std/llm/rerank"

const candidates = [
  {id: "a", answer: "Use a bounded retry loop."},
  {id: "b", answer: "Retry forever until it works."},
  {id: "c", answer: "Fail fast on the first timeout."},
]

const result = pairwise_rerank(candidates, {
  task: "Choose the answer with the safest production behavior.",
  criteria: "Prefer bounded retries, clear failure modes, and low operational risk.",
  model_tier: "small",
})

log(result.ranked[0])
log(result.scores)
```

## pairwise_rerank

`pairwise_rerank(candidates, opts?) -> dict` ranks a list with
`O(n log n)` pairwise comparisons. It returns:

| Field | Type | Description |
|---|---|---|
| `ranked` | list | Candidate values ordered best-first |
| `scores` | list | Per-original-candidate score records: `{index, candidate, wins, losses, ties, comparisons, score, avg_confidence}` |
| `comparisons` | list | Pairwise audit records: `{left_index, right_index, winner, confidence, reasoning}` |

By default, each comparison calls `llm_call_structured` with a small judge
schema. The judge receives `opts.task`, `opts.criteria`, and the two
candidate payloads. `opts.llm_options` can hold generation options; when it is
omitted, normal `llm_call` options on `opts` are used directly.

```harn
import { pairwise_rerank } from "std/llm/rerank"

const items = ["primary source", "unattributed summary", "official documentation"]
const ranked = pairwise_rerank(items, {
  task: "Pick the most relevant search result.",
  criteria: "Prefer direct answers from primary sources.",
  llm_options: {
    model_tier: "small",
    temperature: 0.0,
  },
})
```

For deterministic tests or application-specific scorers, pass
`opts.compare(left, right, ctx)`. The comparator can return a dict, string,
bool, or number:

```harn
import { pairwise_rerank } from "std/llm/rerank"

const ranked = pairwise_rerank(
  ["short", "much longer"],
  {
    compare: { left, right, ctx ->
      if len(left) >= len(right) {
        return {winner: "left", confidence: 1.0}
      }
      return {winner: "right", confidence: 1.0}
    },
  },
)
```

Accepted winners are `left`/`right`/`tie` and aliases such as `A`, `B`,
`first`, `second`, and `equal`. Numeric comparators use positive for left,
negative for right, and zero for a tie.

## self_certainty

`self_certainty(text_or_result, model_opts?) -> float` returns a
length-normalized confidence score in `[0.0, 1.0]` from token log
probabilities:

```harn
import { self_certainty } from "std/llm/rerank"

const score = self_certainty(
  "ignored when logprobs are supplied",
  {
    logprobs: [
      {token: "safe", logprob: -0.10},
      {token: " plan", logprob: -0.20},
    ],
  },
)
```

If a result dict already contains `logprobs`, pass it directly or as the
second argument:

```harn
import { self_certainty } from "std/llm/rerank"

const response = llm_call("Write a short release note.", nil, {
  provider: "openai",
  logprobs: true,
  top_logprobs: 3,
  stream: false,
})

const confidence = self_certainty(response)
```

When no logprobs are supplied, `self_certainty` makes one extra model call
that asks the model to repeat `text_or_result` exactly with `logprobs: true`.
It fails if the provider/model does not return token log probabilities.

Provider support depends on the transport. Harn normalizes OpenAI-compatible
chat-completion logprobs and legacy completion logprobs when providers return
them, including local OpenAI-compatible servers. The mock provider accepts
`llm_mock({text, logprobs: [...]})` for deterministic tests. Anthropic,
Bedrock, Gemini/Vertex, and native Ollama routes currently do not expose a
normalized live logprob surface through `llm_call`, so use supplied logprobs
or an OpenAI-compatible route for `self_certainty`.

The score reflects the model's token-level certainty in generated text, not
factual correctness. Use it as a calibration signal alongside normal
validation, retrieval checks, or pairwise judging.
