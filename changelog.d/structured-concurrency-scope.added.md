- **Structured concurrency: `scope { ... }` nurseries.** Tasks spawned inside a
  `scope { }` block are joined when the block exits — so a spawned task can no
  longer outlive its scope unnoticed, and an error in any of them is no longer
  silently swallowed. At scope exit the first failing task's error propagates
  out of the block (catchable with `try`) after its siblings are cancelled; on a
  `throw`/`return`/`break` out of the block the bound tasks are cancelled rather
  than leaked. Explicitly `await`-ing a task removes it from the nursery (no
  double-join). A bare `spawn` with no enclosing `scope { }` keeps its previous
  detached behavior, so this is additive. `scope` is a contextual keyword —
  existing identifiers, dict keys, and properties named `scope` are unaffected.
