- `jq` / `jq_first`: `and` now binds tighter than `or`, and `.active and .ok` is a
  boolean instead of the path `.active.and.ok`.
- `truncate_head_tail(text, 0)` no longer returns every line after the "omitted" marker.
- `date_now()` and `date_now_iso()` read the mockable clock, agreeing with `now_ms()`
  and `harness.clock.now()`.
- Decimal errors from `sin`, `log2`, `exp`, and the other float-only math builtins name
  the builtin.
- The lexer counts lines after a backslash-newline inside a single-line string, so later
  diagnostics point at the right line.
- Attribute dict keys accept keywords such as `type`, so `harn fmt` output for
  `@attr(m: {"type": ..})` parses again.
- The `unused-import` fix removes the exact unused name instead of the first substring
  match (`import { t, foo }` no longer becomes `impor ...`).
- `blank-line-between-items` sees a multi-line doc block glued to the previous item.
- `harn eval` on a directory with no run records reports that instead of panicking.
- `harn test conformance --determinism --filter` accepts the same `a|b` and glob filters
  as a normal conformance run.
- `harn workflow validate` prints warnings for valid bundles in text mode.
- Docs name `harness.tools.composition_execute`, `harness.agent.compact_transcript`,
  `harness.runtime.flow_evaluate_invariants`, `harness.runtime.pipeline_input`, and the
  `harness.agent.*` session methods instead of removed globals.
