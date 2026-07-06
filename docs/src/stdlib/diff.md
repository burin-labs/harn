# Diff stdlib

`import "std/diff"` exposes line-oriented diff helpers and a structural
review diff backed by hostlib tree-sitter parsing.

## `structural_diff` — syntax-aware review diff

`structural_diff(path_a, path_b, options?)` compares two source files by
syntax tree and returns a human-consumable structure. It is not a patch
format; apply and staging flows should keep using unified line diffs.

`options` may be a language string:

```harn,ignore
import { structural_diff } from "std/diff"

const result = structural_diff("before.rs", "after.rs", "rust")
```

or a dict:

```harn,ignore
const result = structural_diff(
  "before.rs",
  "after.rs",
  {language: "rust", max_bytes: 1048576, max_nodes: 20000, max_graph_edges: 20000000},
)
```

When both files parse and stay under the limits, the result has
`result: "ok"`, `mode: "structural"`, `changed`, `changes`, and a
summary. Each change is one of `insert`, `delete`, `replace`, or `move`
and carries `before` / `after` row-column spans plus small text snippets
for renderer labels.

If the grammar cannot be inferred, parsing reports errors, or a size
limit trips, the function silently returns `result: "fallback"` with
`mode: "line"` and a `line_diff` payload. This lets TUI/GUI renderers
degrade predictably without switching to a mutation-oriented patch path.

## Line helpers

- `diff_lines(before, after)` returns line-level operations and counts.
- `unified_diff(before, after, options?)` renders a unified diff string.
- `colorize_diff(diff_text, options?)` applies ANSI color to unified
  diff text.
- `diff_summary(before, after)` returns compact line-change counts.
- `render_diff_stat(entries, options?)` renders a small per-file stat table.
