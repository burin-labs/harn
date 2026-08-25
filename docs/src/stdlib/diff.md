# Diff stdlib

`import "std/diff"` exposes line-oriented diff helpers and structural
review summaries backed by hostlib tree-sitter parsing.

## `changeset_summary` — symbol-level review header

`changeset_summary(ast, files)` accepts the narrow `HarnessAst` handle and
bounded explicit file images shaped as
`{path, before?, after?}` and returns `harn.review_changeset.v1`. The result
names added, removed, moved, renamed, and signature-changed symbols; separates
structural files from reshaped-only files; and labels name-matched candidate
`CALLS` relations as heuristic.

```harn,ignore
import { changeset_summary } from "std/diff"

fn summarize(ast: HarnessAst, old_source: string, new_source: string) {
  const summary = changeset_summary(ast, [
  {path: "src/lib.rs", before: old_source, after: new_source},
  ])
}
```

Unsupported languages, parse failures, and resource limits produce explicit
degraded file entries instead of semantic guesses.

## `structural_diff` — syntax-aware review diff

`structural_diff(ast, path_a, path_b, options?)` compares two source files by
syntax tree and returns a human-consumable structure. It is not a patch
format; apply and staging flows should keep using unified line diffs.

`options` may be a language string:

```harn,ignore
import { structural_diff } from "std/diff"

fn compare(ast: HarnessAst) {
  const result = structural_diff(ast, "before.rs", "after.rs", "rust")
}
```

or a dict:

```harn,ignore
fn compare_bounded(ast: HarnessAst) {
  const result = structural_diff(
    ast,
    "before.rs",
    "after.rs",
    {
      language: "rust",
      max_bytes: 1048576,
      max_nodes: 20000,
      max_graph_edges: 20000000,
    },
  )
}
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
- `diff_artifact(before, after, options?)` returns change counts and a
  rendered unified diff from one comparison.
- `unified_diff(before, after, options?)` renders a unified diff string.
- `colorize_diff(diff_text, options?)` applies ANSI color to unified
  diff text.
- `diff_summary(before, after)` returns compact line-change counts.
- `render_diff_stat(entries, options?)` renders a small per-file stat table.

All line helpers use Harn's native Histogram diff engine. Histogram diff keeps
structural edits responsive on large or repetitive source files while
producing readable hunks. See the [`similar` algorithm
guide](https://docs.rs/similar/latest/similar/algorithms/index.html) for the
algorithm tradeoffs. Use `diff_artifact` when you need both counts and rendered
output so Harn compares the input once.

Line endings are part of the comparison. If either input lacks its final
newline, the rendered diff includes Git's standard missing-final-newline marker
and the change counts include the affected line.
