# LLM ensemble helpers

`std/llm/ensemble` contains deterministic orchestration helpers for search
patterns around model calls. The helpers are plain Harn functions, so tests can
mock or replace expansion and scoring without touching provider transport.

## tree_of_thoughts

`tree_of_thoughts(opts)` runs bounded Tree-of-Thoughts search over caller-defined
states. It supports breadth-first, depth-first, and beam search.

```harn
import { tree_of_thoughts } from "std/llm/ensemble"

const result = tree_of_thoughts({
  initial_state: {steps: [], value: 0},
  search: "beam",
  k: 3,
  beam_width: 2,
  max_depth: 4,
  expand: { state, k ->
    return [
      {steps: state.steps.push("+1"), value: state.value + 1},
      {steps: state.steps.push("+2"), value: state.value + 2},
      {steps: state.steps.push("+3"), value: state.value + 3},
    ].take(k)
  },
  evaluate: { state -> state.value },
  is_terminal: { state -> state.value >= 7 },
})

if result.ok {
  log(result.best_path.last().steps)
}
```

### Options

| Key | Type | Default | Description |
|---|---|---|---|
| `initial_state` | any | required | Root search state |
| `expand` | closure | required | Called as `expand(state, k)` and must return a list of next states |
| `evaluate` | closure | required | Called as `evaluate(state)` and must return an int or float score; higher is better |
| `is_terminal` | closure | required | Called as `is_terminal(state)` and must return a bool |
| `search` | string | `"bfs"` | One of `"bfs"`, `"dfs"`, or `"beam"` |
| `k` | int | `1` | Maximum number of expanded states consumed per node |
| `beam_width` | int | `k` | Number of candidates retained at each beam-search depth |
| `max_depth` | int | `8` | Maximum child depth to expand |
| `stop` | closure | nil | Optional `stop(node) -> bool` predicate for ending search early |

### Return value

The result is a dict with:

| Field | Description |
|---|---|
| `ok` | True when at least one terminal state was found |
| `best_path` | List of states from the root to the best terminal state, or the best explored state when no terminal was found |
| `best_score` | Score for `best_path`'s final state |
| `best_node` | Node dict for the selected state |
| `stop_reason` | `"terminal"`, `"stop"`, or `"exhausted"` |
| `tree` | Flat tree metadata: `{root_id, best_id, nodes, search, k, max_depth, beam_width}` |

Each tree node is `{id, parent_id, state, path, depth, score, terminal}`.
